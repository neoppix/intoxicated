//! Join links: `intoxicated <placeId>`, `roblox://` links, roblox.com's
//! browser link and roblox.com `/games/` web links.
//!
//! A link is hostile input. It came from a browser click or a command line,
//! and the browser form carries a one-time sign-in ticket. So:
//!
//! - [`validate`] decides everything before the engine is reached. It turns
//!   every accepted form into one link in the engine's own grammar,
//!   `roblox://experiences/start?placeId=<id>[&...]`, and refuses the rest. A
//!   refused link is an error at the command line (exit 2) or in the devctl
//!   reply; nothing of it is delivered.
//! - The ticket (`gameinfo`) is never copied out of the link it arrived in,
//!   so it can reach neither the engine nor a log.
//! - The server-selecting values (`gameInstanceId`, `accessCode`, `linkCode`,
//!   `reservedServerAccessCode`) are checked for charset and length before
//!   they are passed on.
//! - The only rendering of a link that is ever printed is [`describe`]: its
//!   scheme, its parameter names and its length.
//!
//! Delivery on 2.748, measured (see the in-game runs in the commit that added
//! this file): `JNIBaseUrlProtocol.init` is never called, because with it the
//! engine ignores every later link; the link is held until the signed-in home
//! page reports `HOME_PAGE_INTERACTIVE` and is then published once on
//! `Linking.detectURL`. The engine answers with a `Game.launch` for the place.
//! Only a `Game.launch` for the requested place, after the publish, confirms
//! the link. Any other `Game.launch` means a join is already under way, so the
//! link is dropped rather than published (again), which is what keeps a join
//! from ever being doubled. `INTOXICATED_DEEPLINK_RETRY` adds a bounded
//! re-publish for a link nothing answered.
//!
//! Threads: engine notifications and bus payloads arrive on engine threads and
//! only set atomics or push to a small queue. Every engine call made here runs
//! on the thread that runs bring-up and then the looper pump.

use std::ffi::{c_char, c_void, CStr};
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::mpsc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use intoxicated_linker_sys as linker;

use crate::bus;

/// The longest link taken, in bytes.
pub const LIMIT: usize = 2048;

/// Bus topics and payload key, read from the running 2.748 engine
/// (`JNILinkingProtocol.getDetectURLId`, `JNIExperienceProtocol.getLaunchId`,
/// `JNILinkingProtocol.getUrlKey`).
const DETECT_URL: &str = "Linking.detectURL";
const GAME_LAUNCH: &str = "Game.launch";

const BASE_URL_CLASS: &str = "com/roblox/universalapp/linking/JNIBaseUrlProtocol";
const WEB_LOGIN_CLASS: &str = "com/roblox/universalapp/linking/JNIWebLoginProtocol";
const BASE_URL_COLD_START: &str =
    "Java_com_roblox_universalapp_linking_JNIBaseUrlProtocol_maybeHandleColdStartProtocolLaunch";
const WEB_LOGIN_COLD_START: &str =
    "Java_com_roblox_universalapp_linking_JNIWebLoginProtocol_maybeHandleColdStartProtocolLaunch";
const WEB_LOGIN_INIT: &str = "Java_com_roblox_universalapp_linking_JNIWebLoginProtocol_init";
const NATIVE_GL_CLASS: &str = "com/roblox/engine/jni/NativeGLInterface";
const IS_COLD_START_TO_GAME: &str = "Java_com_roblox_engine_jni_NativeGLInterface_isColdStartDeeplinkToGame";

/// Present (any value): re-publish a link nothing answered. The launcher sets
/// it whenever it passes a link.
pub const RETRY_VARIABLE: &str = "INTOXICATED_DEEPLINK_RETRY";
/// Present (any value): hold the link but never publish it. The control for
/// "the publish is what carries the link"; such a run stays on the home page.
pub const NO_PUBLISH_VARIABLE: &str = "INTOXICATED_DEEPLINK_NO_PUBLISH";
/// Present (any value): devctl's `bussub` prints payloads instead of their
/// length. Payloads can carry identities and private-server codes.
pub const PROBE_VARIABLE: &str = "INTOXICATED_DEEPLINK_PROBE";

/// How long a published link may go unanswered before the verdict (or the
/// first retry).
const VERDICT_WINDOW: Duration = Duration::from_secs(15);
const RETRY_INTERVAL: Duration = Duration::from_millis(1000);
const MAX_RETRIES: u32 = 40;

const SUCCESS: &str =
    "[deeplink] the app shell asked to launch an experience (Game.launch); the link reached the engine";
const ALREADY_LAUNCHING: &str = "[deeplink] the app shell is already launching this place (Game.launch before \
     the link was published); the link is dropped, not published";
const OTHER_PLACE: &str = "[deeplink] the app shell asked to launch a different place (Game.launch); that does \
     not confirm this link, and the link is dropped so the two joins cannot collide";
const UNKNOWN_PLACE: &str = "[deeplink] a Game.launch arrived whose place id could not be read; it does not \
     confirm this link, and the link is dropped so the two joins cannot collide";
const JOIN_UNDER_WAY: &str =
    "[deeplink] the engine's log shows a join under way; the link is not published again";
const SIGN_IN_PAGE: &str = "[deeplink] the app shell is on the sign-in page; the link is held and will be \
     published once you are signed in and the home page is up";
const VERDICT_NO_SUBSCRIPTION: &str = "[deeplink] the link was published, and with no subscription to \
     Game.launch nothing here can say whether the app shell asked to launch an experience";
const VERDICT_REGISTERED: &str =
    "[deeplink] the engine registered a deep link, but has not asked to launch an experience";
const VERDICT_NOTHING: &str = "[deeplink] the home page is up and nothing asked to launch an experience -- \
     this link did not reach an experience";

// ------------------------------------------------------------------ the gate

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scheme {
    Roblox,
    RobloxPlayer,
    Https,
}

/// The first checks every link goes through: length, charset, scheme.
fn gate(raw: &str) -> Result<Scheme, String> {
    if raw.is_empty() {
        return Err("--join-url was given an empty string".into());
    }
    if raw.len() > LIMIT {
        return Err(format!("--join-url is {} bytes and the limit is {LIMIT}", raw.len()));
    }
    if let Some(at) = raw.bytes().position(|b| !(0x21..=0x7e).contains(&b)) {
        return Err(format!("--join-url has a character that is not printable ASCII at byte {at}"));
    }
    let Some((scheme, _)) = raw.split_once(':') else {
        return Err("--join-url has no scheme".into());
    };
    match scheme.to_ascii_lowercase().as_str() {
        "roblox" => Ok(Scheme::Roblox),
        "roblox-player" => Ok(Scheme::RobloxPlayer),
        "https" => Ok(Scheme::Https),
        other => Err(format!(
            "--join-url has scheme \"{}\"; Intoxicated handles roblox, roblox-player and https roblox.com links",
            backslash_quotes(other)
        )),
    }
}

fn backslash_quotes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// The text after the first `:`, with every leading `/` removed.
fn after_scheme(link: &str) -> &str {
    link.split_once(':').map_or("", |(_, rest)| rest).trim_start_matches('/')
}

/// roblox.com's desktop launcher form, `roblox-player:1+launchmode:play+...`:
/// the version digits, then `+`-separated `name:value` tokens.
fn is_desktop_form(link: &str) -> bool {
    let Some((scheme, _)) = link.split_once(':') else { return false };
    if !scheme.eq_ignore_ascii_case("roblox-player") {
        return false;
    }
    let rest = after_scheme(link);
    match rest.split_once('+') {
        Some((version, _)) => !version.is_empty() && version.bytes().all(|b| b.is_ascii_digit()),
        None => false,
    }
}

// ------------------------------------------------------------ description

/// A parameter name that may be printed: 1 to 40 of `A-Z a-z 0-9 _ -`.
fn printable_name(name: &str) -> bool {
    (1..=40).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// The only rendering of a link that may be printed anywhere: its scheme, the
/// names of its parameters and its length. Never a value, host or path.
pub fn describe(link: &str) -> String {
    let scheme = link.split_once(':').map_or(link, |(s, _)| s).to_ascii_lowercase();
    let rest = after_scheme(link);
    let names: Vec<&str> = if is_desktop_form(link) {
        rest.split('+').filter_map(|t| t.split_once(':').map(|(n, _)| n)).collect()
    } else {
        rest.split(['&', '?']).filter_map(|t| t.split_once('=').map(|(n, _)| n)).collect()
    };
    let mut shown: Vec<&str> = Vec::new();
    for name in names.into_iter().filter(|n| printable_name(n)) {
        if shown.last() != Some(&name) {
            shown.push(name);
        }
    }
    if shown.is_empty() {
        format!("{scheme}:// ({} bytes, no parameters)", link.len())
    } else {
        format!("{scheme}:// with {} ({} bytes; values not shown)", shown.join(", "), link.len())
    }
}

// ------------------------------------------------------------- the values

/// A place id: 1 to 20 ASCII digits.
fn is_place_id(value: &str) -> bool {
    (1..=20).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_digit())
}

/// A server (job) id: a UUID, `8-4-4-4-12` hexadecimal digits.
fn is_server_id(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

/// A private-server code: 1 to 64 of `A-Z a-z 0-9 _ -`, the charset the
/// engine's own link pattern allows for `accessCode` and `linkCode`.
fn is_code(value: &str) -> bool {
    (1..=64).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

const SERVER_ID_RULE: &str = "is not a server id (a UUID, 8-4-4-4-12 hexadecimal digits)";
const CODE_RULE: &str = "is not a private-server code (1 to 64 of A-Z a-z 0-9 _ -)";

/// `value` without leading zeros, which is how the engine reads a place id.
fn normalise_place(value: &str) -> String {
    let trimmed = value.trim_start_matches('0');
    if trimmed.is_empty() { "0".into() } else { trimmed.into() }
}

/// The server-selecting values a link may carry, in the engine's spelling.
///
/// `gameInstanceId` is measured on 2.748: the engine joins the server it
/// names, and one that does not exist ends on the engine's "This experience
/// has ended" screen instead of a matchmade server. That the engine joins the
/// private server for `accessCode`, `linkCode` (roblox.com's
/// `privateServerLinkCode`) or `reservedServerAccessCode` is INFERRED from its
/// own link pattern, which names all three; no private server was tried.
#[derive(Default)]
struct Server {
    game_instance_id: Option<String>,
    access_code: Option<String>,
    link_code: Option<String>,
    reserved_server_access_code: Option<String>,
}

impl Server {
    fn query(&self) -> String {
        let mut out = String::new();
        for (name, value) in [
            ("gameInstanceId", &self.game_instance_id),
            ("accessCode", &self.access_code),
            ("linkCode", &self.link_code),
            ("reservedServerAccessCode", &self.reserved_server_access_code),
        ] {
            if let Some(value) = value {
                out.push('&');
                out.push_str(name);
                out.push('=');
                out.push_str(value);
            }
        }
        out
    }
}

/// Records `value` under `name`, refusing a second one.
fn once(slot: &mut Option<String>, value: &str, twice: impl FnOnce() -> String) -> Result<(), String> {
    if slot.is_some() {
        return Err(twice());
    }
    *slot = Some(value.to_string());
    Ok(())
}

/// Checks the engine-grammar parameters of a query (`k=v&k=v`): the place id
/// and the server-selecting values. Everything else is the engine's to read.
/// Returns the place id as given.
fn check_engine_params(query: &str) -> Result<String, String> {
    let mut place = None;
    let mut server = Server::default();
    for piece in query.split('&') {
        let Some((key, value)) = piece.split_once('=') else { continue };
        let (slot, name, valid, rule) = match key.to_ascii_lowercase().as_str() {
            "placeid" => (&mut place, "placeId", is_place_id(value), ""),
            "gameinstanceid" => (&mut server.game_instance_id, "gameInstanceId", is_server_id(value), SERVER_ID_RULE),
            "accesscode" => (&mut server.access_code, "accessCode", is_code(value), CODE_RULE),
            "linkcode" => (&mut server.link_code, "linkCode", is_code(value), CODE_RULE),
            "reservedserveraccesscode" => {
                (&mut server.reserved_server_access_code, "reservedServerAccessCode", is_code(value), CODE_RULE)
            }
            _ => continue,
        };
        once(slot, value, || format!("this link names {name} more than once"))?;
        if !valid {
            return Err(if name == "placeId" {
                "this link's placeId is not a number, and Intoxicated will not invent one".into()
            } else {
                format!("this link's {name} {rule}")
            });
        }
    }
    place.ok_or_else(|| "this link names no placeId, so there is nothing to join".into())
}

// ------------------------------------------------------------ translation

/// How the delivered link came about, for the delivery report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Form {
    /// A `roblox://` link, delivered exactly as given.
    Engine,
    /// A `roblox://` link with its scheme or slashes respelled.
    Respelled,
    /// A `roblox-player://` query link, delivered as `roblox://`.
    RobloxPlayer,
    /// roblox.com's desktop launcher link.
    Desktop,
    /// A roblox.com `/games/` web link.
    Web,
}

struct Target {
    link: String,
    place: String,
    form: Form,
    dropped: Vec<String>,
    ticket: bool,
}

fn push_name(names: &mut Vec<String>, name: &str) {
    if printable_name(name) && !names.iter().any(|n| n == name) {
        names.push(name.to_string());
    }
}

/// A `roblox://` or `roblox-player://` link already in the engine's grammar:
/// checked, then delivered as `roblox://` with its text otherwise unchanged.
fn translate_query(raw: &str, scheme: Scheme) -> Result<Target, String> {
    let rest = after_scheme(raw);
    let query = rest.split_once('?').map_or(rest, |(_, q)| q);
    let place = check_engine_params(query)?;
    let link = format!("roblox://{rest}");
    let form = match scheme {
        Scheme::RobloxPlayer => Form::RobloxPlayer,
        _ if link == raw => Form::Engine,
        _ => Form::Respelled,
    };
    Ok(Target { link, place, form, dropped: Vec::new(), ticket: false })
}

/// Percent-decodes `text` once. Every escape must be two hexadecimal digits
/// for a printable character; `+` stays `+`.
fn percent_decode(text: &str) -> Result<String, &'static str> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'%' {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        if bytes.len() - i < 3 {
            return Err("its placelauncherurl ends in a percent escape that is cut short");
        }
        let hex = |b: u8| char::from(b).to_digit(16);
        let (Some(high), Some(low)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) else {
            return Err("its placelauncherurl has a percent escape that is not two hexadecimal digits");
        };
        let byte = (high * 16 + low) as u8;
        if !(0x20..=0x7e).contains(&byte) {
            return Err("its placelauncherurl has a percent escape for a character that is not printable");
        }
        out.push(byte);
        i += 3;
    }
    // Printable ASCII in, printable ASCII out.
    Ok(String::from_utf8(out).expect("printable ASCII is UTF-8"))
}

/// roblox.com's desktop launcher link:
/// `roblox-player:1+launchmode:play+gameinfo:<ticket>+launchtime:<n>+placelauncherurl:<url>+...`.
///
/// The place, and the server it names if it names one, come out of the
/// percent-encoded `placelauncherurl`. The `gameinfo` ticket is only noted as
/// present: its value is never read into anything.
fn translate_desktop(raw: &str) -> Result<Target, String> {
    let mut launch_mode = None;
    let mut launcher_url = None;
    let mut ticket = false;
    for token in after_scheme(raw).split('+') {
        let Some((key, value)) = token.split_once(':') else { continue };
        match key.to_ascii_lowercase().as_str() {
            "launchmode" => launch_mode = Some(value),
            "placelauncherurl" => launcher_url = Some(value),
            "gameinfo" => ticket = true,
            _ => {}
        }
    }
    if launch_mode.is_some_and(|m| !m.eq_ignore_ascii_case("play")) {
        return Err("this link's launchmode is not play, so it is not a request to join an experience".into());
    }
    let Some(launcher_url) = launcher_url else {
        return Err("this link carries no placelauncherurl, so there is no place id in it to translate".into());
    };
    let decoded = percent_decode(launcher_url)?;
    let query = decoded.split_once('?').map_or("", |(_, q)| q);

    let mut place = None;
    let mut server = Server::default();
    let mut dropped = Vec::new();
    for piece in query.split('&') {
        let Some((key, value)) = piece.split_once('=') else { continue };
        let (slot, name, valid, rule) = match key.to_ascii_lowercase().as_str() {
            "placeid" => (&mut place, "placeId", is_place_id(value), ""),
            // `RequestGameJob` names its server `gameId`.
            "gameid" | "jobid" | "gameinstanceid" => {
                (&mut server.game_instance_id, "gameId", is_server_id(value), SERVER_ID_RULE)
            }
            "accesscode" => (&mut server.access_code, "accessCode", is_code(value), CODE_RULE),
            "linkcode" => (&mut server.link_code, "linkCode", is_code(value), CODE_RULE),
            "reservedserveraccesscode" => {
                (&mut server.reserved_server_access_code, "reservedServerAccessCode", is_code(value), CODE_RULE)
            }
            _ => {
                push_name(&mut dropped, key);
                continue;
            }
        };
        once(slot, value, || format!("its placelauncherurl names {name} more than once"))?;
        if !valid {
            return Err(if name == "placeId" {
                "its placelauncherurl's placeId is not a number, and Intoxicated will not invent one".into()
            } else {
                format!("its placelauncherurl's {name} {rule}")
            });
        }
    }
    let Some(place) = place else {
        return Err("its placelauncherurl names no placeId, so there is nothing to join".into());
    };
    let link = format!("roblox://experiences/start?placeId={place}{}", server.query());
    Ok(Target { link, place, form: Form::Desktop, dropped, ticket })
}

/// The hosts a roblox.com game link is served from.
const WEB_HOSTS: [&str; 4] = ["roblox.com", "www.roblox.com", "web.roblox.com", "m.roblox.com"];

/// A locale segment the site puts before `/games`: `de` or `en-us`, the
/// shape the engine's own pattern allows.
fn is_locale(segment: &str) -> bool {
    let b = segment.as_bytes();
    let two = |s: &[u8]| s.len() == 2 && s.iter().all(u8::is_ascii_alphabetic);
    match segment.split_once('-') {
        None => two(b),
        Some((lang, region)) => {
            two(lang.as_bytes())
                && (2..=3).contains(&region.len())
                && region.bytes().all(|c| c.is_ascii_alphanumeric())
        }
    }
}

/// A roblox.com web link: `https://www.roblox.com/games/<id>[/<name>]`, with
/// an optional locale before `/games` and an optional
/// `?privateServerLinkCode=<code>`, or `https://www.roblox.com/games/start?<engine query>`.
fn translate_web(raw: &str) -> Result<Target, String> {
    const NOT_GAMES: &str = "this https link is not a roblox.com /games/ link";
    let after = raw.split_once(':').map_or("", |(_, rest)| rest);
    let Some(rest) = after.strip_prefix("//") else { return Err(NOT_GAMES.into()) };
    let host_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let host = rest[..host_end].to_ascii_lowercase();
    if !WEB_HOSTS.contains(&host.as_str()) {
        return Err("this https link is not to roblox.com".into());
    }
    let tail = rest[host_end..].split('#').next().unwrap_or("");
    let (path, query) = tail.split_once('?').unwrap_or((tail, ""));
    let mut segments = path.split('/').filter(|s| !s.is_empty()).peekable();
    if segments.peek().is_some_and(|s| is_locale(s)) {
        segments.next();
    }
    if !segments.next().is_some_and(|s| s.eq_ignore_ascii_case("games")) {
        return Err(NOT_GAMES.into());
    }
    match segments.next() {
        Some(s) if s.eq_ignore_ascii_case("start") => {
            if segments.next().is_some() {
                return Err(NOT_GAMES.into());
            }
            let place = check_engine_params(query)?;
            let link = format!("roblox://experiences/start?{query}");
            Ok(Target { link, place, form: Form::Web, dropped: Vec::new(), ticket: false })
        }
        Some(id) if !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()) => {
            if !is_place_id(id) {
                return Err("this link's place id is not a number, and Intoxicated will not invent one".into());
            }
            let mut server = Server::default();
            let mut dropped = Vec::new();
            for piece in query.split('&') {
                let Some((key, value)) = piece.split_once('=') else { continue };
                if key.eq_ignore_ascii_case("privateServerLinkCode") {
                    once(&mut server.link_code, value, || {
                        "this link names privateServerLinkCode more than once".into()
                    })?;
                    if !is_code(value) {
                        return Err(format!("this link's privateServerLinkCode {CODE_RULE}"));
                    }
                } else {
                    push_name(&mut dropped, key);
                }
            }
            let link = format!("roblox://experiences/start?placeId={id}{}", server.query());
            Ok(Target { link, place: id.to_string(), form: Form::Web, dropped, ticket: false })
        }
        _ => Err(NOT_GAMES.into()),
    }
}

// ------------------------------------------------------------- the result

/// A join link that passed [`validate`], in the form the engine is given.
///
/// The only way to get one is [`validate`], so nothing that was refused can
/// reach [`deliver`] or a publish. `Debug` prints the description, never the
/// text: the delivered link can carry private-server codes.
pub struct JoinUrl {
    delivered: String,
    place: String,
    arrived: String,
    form: Form,
    dropped: Vec<String>,
    ticket: bool,
}

impl JoinUrl {
    /// The link the engine is given. For the engine boundary and the
    /// launcher's check only; never print it.
    pub fn delivered(&self) -> &str {
        &self.delivered
    }

    /// The requested place, without leading zeros.
    pub fn place_id(&self) -> &str {
        &self.place
    }

    /// The lines printed when the link is delivered.
    fn report(&self) -> Vec<String> {
        let mut out = vec![format!("[deeplink] delivering {}", self.arrived)];
        let result = describe(&self.delivered);
        match self.form {
            Form::Engine => {}
            Form::Respelled => out.push(format!("[deeplink] translated the link to {result}")),
            Form::RobloxPlayer => out.push(format!("[deeplink] translated the roblox-player link to {result}")),
            Form::Desktop => out.push(format!("[deeplink] translated the desktop link to {result}")),
            Form::Web => out.push(format!("[deeplink] translated the roblox.com web link to {result}")),
        }
        if self.ticket {
            out.push("[deeplink] its one-time sign-in ticket (gameinfo) is dropped; it never reaches the engine".into());
        }
        if !self.dropped.is_empty() {
            let whose = if self.form == Form::Desktop { "its launcher URL" } else { "the web link" };
            out.push(format!(
                "[deeplink] {whose} also carried {}, which Intoxicated does not carry across",
                self.dropped.join(", ")
            ));
        }
        out
    }
}

impl fmt::Debug for JoinUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "JoinUrl({})", describe(&self.delivered))
    }
}

/// Checks `raw` and turns it into the link the engine is given, or says why
/// it is refused. Applied to `--join-url`, to devctl's `deeplink` and to the
/// launcher's `--check-join-url`.
///
/// Accepted: `roblox://` and `roblox-player://` links in the engine's grammar,
/// roblox.com's desktop launcher link, and https roblox.com `/games/` links.
/// Each must name a place; server-selecting values must be well formed. The
/// error never contains a value from the link.
pub fn validate(raw: &str) -> Result<JoinUrl, String> {
    let scheme = gate(raw)?;
    let target = match scheme {
        Scheme::RobloxPlayer if is_desktop_form(raw) => translate_desktop(raw),
        Scheme::Https => translate_web(raw),
        _ => translate_query(raw, scheme),
    }
    .map_err(|reason| format!("--join-url was refused: {reason}"))?;
    match gate(&target.link) {
        Ok(Scheme::Roblox) => {}
        Ok(_) => unreachable!("every translation produces a roblox:// link"),
        Err(e) => {
            return Err(format!(
                "--join-url was refused: the link it translates to is not one Intoxicated takes: {e}"
            ))
        }
    }
    Ok(JoinUrl {
        place: normalise_place(&target.place),
        arrived: describe(raw),
        delivered: target.link,
        form: target.form,
        dropped: target.dropped,
        ticket: target.ticket,
    })
}

/// `{"url":"<link>"}`, with `"` and `\` escaped. The link is printable ASCII,
/// so nothing else needs escaping.
fn detect_url_payload(link: &str) -> String {
    format!("{{\"url\":\"{}\"}}", backslash_quotes(link))
}

// --------------------------------------------------------- the held link

/// Which place a `Game.launch` named.
#[derive(Clone, Debug, PartialEq, Eq)]
enum LaunchPlace {
    Place(String),
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Launch {
    place: LaunchPlace,
    /// Whether it arrived after this link's first publish.
    after_publish: bool,
}

/// The place in a `Game.launch` payload, `{"placeId":<number>,...}`.
fn launch_place(payload: &[u8]) -> LaunchPlace {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(payload) else { return LaunchPlace::Unknown };
    match value.get("placeId") {
        Some(serde_json::Value::Number(n)) => n.as_u64().map_or(LaunchPlace::Unknown, |n| LaunchPlace::Place(n.to_string())),
        Some(serde_json::Value::String(s)) if is_place_id(s) => LaunchPlace::Place(normalise_place(s)),
        _ => LaunchPlace::Unknown,
    }
}

/// The engine calls the held link makes after bring-up.
trait Engine {
    /// `publishRaw("Linking.detectURL", json)`; `None` when not exported.
    fn publish(&mut self, json: &str) -> Option<Result<(), String>>;
    /// `isColdStartDeeplinkToGame()`; `None` when not exported.
    fn cold_start_to_game(&mut self) -> Option<Result<bool, String>>;
}

/// What the engine reported since the last tick.
#[derive(Default)]
struct Inputs {
    home_up: bool,
    /// `APP_READY Landing` notifications so far.
    landings: u32,
    launches: Vec<Launch>,
    /// Joins the engine's own log has reported so far.
    joins: u64,
}

/// The held link, stages B to E. Pure apart from `Engine`, so it can be
/// driven with a fake clock and a fake engine.
struct Machine {
    payload: String,
    place: String,
    live: bool,
    retry: bool,
    no_publish: bool,
    joins_at_hold: u64,
    published: bool,
    publishes: u32,
    verdict_from: Option<Instant>,
    retries: u32,
    last_retry: Option<Instant>,
    landings_seen: u32,
    done: bool,
}

impl Machine {
    fn new(url: &JoinUrl, joins_at_hold: u64) -> Self {
        Machine {
            payload: detect_url_payload(&url.delivered),
            place: url.place.clone(),
            live: false,
            retry: false,
            no_publish: false,
            joins_at_hold,
            published: false,
            publishes: 0,
            verdict_from: None,
            retries: 0,
            last_retry: None,
            landings_seen: 0,
            done: false,
        }
    }

    fn publish(&mut self, phase: &str, engine: &mut dyn Engine, out: &mut Vec<String>) {
        self.publishes += 1;
        if self.no_publish {
            out.push(format!("[deeplink] ({phase}) not publishing ({NO_PUBLISH_VARIABLE})"));
            return;
        }
        out.push(match engine.publish(&self.payload) {
            None => "[deeplink] MessageBus.publishRaw is not exported by this build".into(),
            Some(Ok(())) => format!("[deeplink] ({phase}) published Linking.detectURL"),
            Some(Err(e)) => format!("[deeplink] ({phase}) publishing Linking.detectURL failed: {e}"),
        });
    }

    fn verdict(&self, engine: &mut dyn Engine) -> &'static str {
        if !self.live {
            VERDICT_NO_SUBSCRIPTION
        } else if matches!(engine.cold_start_to_game(), Some(Ok(true))) {
            VERDICT_REGISTERED
        } else {
            VERDICT_NOTHING
        }
    }

    /// One tick. Returns the lines to print; sets `done` when the link is
    /// finished with, one way or another.
    fn step(&mut self, now: Instant, inputs: &Inputs, engine: &mut dyn Engine) -> Vec<String> {
        let mut out = Vec::new();
        if self.done {
            return out;
        }
        // Any launch settles it. Only this place, after the publish, is a
        // confirmation; anything else is a join already under way, which a
        // publish now would double.
        if let Some(launch) = inputs.launches.first() {
            out.push(
                match &launch.place {
                    LaunchPlace::Place(p) if *p == self.place && launch.after_publish => SUCCESS,
                    LaunchPlace::Place(p) if *p == self.place => ALREADY_LAUNCHING,
                    LaunchPlace::Place(_) => OTHER_PLACE,
                    LaunchPlace::Unknown => UNKNOWN_PLACE,
                }
                .into(),
            );
            self.done = true;
            return out;
        }
        if !self.published {
            while self.landings_seen < inputs.landings {
                self.landings_seen += 1;
                out.push(SIGN_IN_PAGE.into());
            }
            if inputs.home_up {
                self.publish("home page interactive", engine, &mut out);
                self.published = true;
                self.verdict_from = Some(now);
            }
            return out;
        }
        let Some(from) = self.verdict_from else { return out };
        if now.duration_since(from) < VERDICT_WINDOW {
            return out;
        }
        if self.retry && self.retries < MAX_RETRIES {
            if inputs.joins > self.joins_at_hold {
                out.push(JOIN_UNDER_WAY.into());
                self.done = true;
                return out;
            }
            if self.last_retry.is_none_or(|t| now.duration_since(t) >= RETRY_INTERVAL) {
                self.publish("retry", engine, &mut out);
                self.retries += 1;
                self.last_retry = Some(now);
                out.push(format!("[deeplink] re-published the link (retry {}/{MAX_RETRIES})", self.retries));
            }
            return out;
        }
        out.push(self.verdict(engine).into());
        self.done = true;
        out
    }
}

// ------------------------------------------------------------ live state

const HELD_BIT: u8 = 1;
const DEV_BIT: u8 = 2;

/// Whether `tick` has anything to do. Zero on every ordinary launch, which
/// makes the tick one atomic load.
static WORK: AtomicU8 = AtomicU8::new(0);

struct Held {
    machine: Machine,
    publish_raw: Option<usize>,
    cold_start_query: Option<usize>,
}

static HELD: Mutex<Option<Held>> = Mutex::new(None);
static DELIVERED: AtomicBool = AtomicBool::new(false);

// Written by engine threads, read by the tick.
static HOME_UP: AtomicBool = AtomicBool::new(false);
static LANDINGS: AtomicU32 = AtomicU32::new(0);
/// Publishes of the held link begun so far. Raised before the call, because
/// the engine answers from inside it.
static PUBLISHES: AtomicU32 = AtomicU32::new(0);
/// Never held across an engine call.
static LAUNCHES: Mutex<Vec<Launch>> = Mutex::new(Vec::new());

extern "C" fn on_app_ready(state: *const c_char) {
    if state.is_null() {
        return;
    }
    // SAFETY: the native hands a NUL-terminated string valid for the call.
    if unsafe { CStr::from_ptr(state) }.to_bytes() == b"Landing" {
        LANDINGS.fetch_add(1, Ordering::AcqRel);
    }
}

extern "C" fn on_home_interactive() {
    HOME_UP.store(true, Ordering::Release);
}

extern "C" fn on_game_launch(payload: *const c_char) {
    if WORK.load(Ordering::Acquire) & HELD_BIT == 0 {
        return;
    }
    let after_publish = PUBLISHES.load(Ordering::Acquire) > 0;
    // SAFETY: the bus hands a NUL-terminated string valid for the call, or null.
    let place = unsafe { bus::payload_bytes(payload) }.map_or(LaunchPlace::Unknown, launch_place);
    if let Ok(mut launches) = LAUNCHES.lock() {
        launches.push(Launch { place, after_publish });
    }
}

fn take_launches() -> Vec<Launch> {
    std::mem::take(&mut *LAUNCHES.lock().unwrap_or_else(|p| p.into_inner()))
}

struct LiveEngine {
    publish_raw: Option<usize>,
    cold_start_query: Option<usize>,
}

impl Engine for LiveEngine {
    fn publish(&mut self, json: &str) -> Option<Result<(), String>> {
        let f = self.publish_raw? as *mut c_void;
        PUBLISHES.fetch_add(1, Ordering::AcqRel);
        Some(bus::publish(f, DETECT_URL, json))
    }

    fn cold_start_to_game(&mut self) -> Option<Result<bool, String>> {
        let f = self.cold_start_query? as *mut c_void;
        Some(linker::game_activity::call_static_bare_bool(f, NATIVE_GL_CLASS))
    }
}

/// What stage A ended with. `Debug` is what `intoxicated-run` prints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Held, to be published once the home page is interactive.
    Published,
    /// A cold-start native took the link; nothing is held.
    Claimed(&'static str),
    /// No native that could carry a link is exported.
    NoSurface,
    /// A link was already delivered in this process; this one is not.
    AlreadyHolding,
}

/// Offers the link to a protocol's cold-start native. `Some` when it took it.
fn offer(native: Option<*mut c_void>, short: &'static str, class: &str, link: &str) -> Option<&'static str> {
    let Some(f) = native else {
        println!("[deeplink] {short}.maybeHandleColdStartProtocolLaunch is not exported");
        return None;
    };
    match linker::game_activity::cold_start_protocol_launch(f, class, link) {
        Ok(true) => {
            println!("[deeplink] {short} took the link");
            Some(short)
        }
        Ok(false) => {
            println!("[deeplink] {short} did not claim this link");
            None
        }
        Err(e) => {
            println!("[deeplink] {short} failed: {e}");
            None
        }
    }
}

/// Stage A: called once during bring-up, after `nativeAppBridgeV2InitWithParams`
/// and the session restore, before the Lua app shell starts. Publishes
/// nothing; [`tick`] does that once the home page is up.
pub fn deliver(lib: linker::Library, url: &JoinUrl) -> Outcome {
    for line in url.report() {
        println!("{line}");
    }
    if DELIVERED.swap(true, Ordering::AcqRel) {
        println!("[deeplink] a link was already delivered in this process; this one is not");
        return Outcome::AlreadyHolding;
    }
    let link = url.delivered();
    println!("[deeplink] not calling JNIBaseUrlProtocol.init: on this engine it stops every later link being acted on");
    let base = lib.symbol(BASE_URL_COLD_START);
    if let Some(claimed) = offer(base, "JNIBaseUrlProtocol", BASE_URL_CLASS, link) {
        return Outcome::Claimed(claimed);
    }
    if let Some(init) = lib.symbol(WEB_LOGIN_INIT) {
        match linker::game_activity::protocol_init(init, WEB_LOGIN_CLASS) {
            Ok(()) => println!("[deeplink] JNIWebLoginProtocol.init ok"),
            Err(e) => println!("[deeplink] JNIWebLoginProtocol.init failed: {e}"),
        }
    }
    let web = lib.symbol(WEB_LOGIN_COLD_START);
    if let Some(claimed) = offer(web, "JNIWebLoginProtocol", WEB_LOGIN_CLASS, link) {
        return Outcome::Claimed(claimed);
    }
    let publish_raw = lib.symbol(bus::PUBLISH_RAW);
    if base.is_none() && web.is_none() && publish_raw.is_none() {
        return Outcome::NoSurface;
    }

    let mut machine = Machine::new(url, crate::engine_log::joins_seen());
    machine.no_publish = std::env::var_os(NO_PUBLISH_VARIABLE).is_some();
    *HELD.lock().unwrap_or_else(|p| p.into_inner()) = Some(Held {
        machine,
        publish_raw: publish_raw.map(|f| f as usize),
        cold_start_query: lib.symbol(IS_COLD_START_TO_GAME).map(|f| f as usize),
    });
    WORK.fetch_or(HELD_BIT, Ordering::AcqRel);
    linker::game_activity::app_ready_set_sink(Some(on_app_ready));
    linker::game_activity::home_interactive_set_sink(Some(on_home_interactive));

    let live = match lib.symbol(bus::DO_SUBSCRIBE_RAW) {
        None => {
            println!("[deeplink] MessageBus.doSubscribeRaw is not exported; a launch cannot be seen");
            false
        }
        Some(f) => match bus::subscribe(f, GAME_LAUNCH, Some(on_game_launch)) {
            Err(e) => {
                println!("[deeplink] could not subscribe to Game.launch ({e}); a launch cannot be seen");
                false
            }
            Ok(_) => match bus::connected(lib.symbol(bus::IS_CONNECTED), GAME_LAUNCH) {
                Some(false) => {
                    println!("[deeplink] subscribed to Game.launch, but the bus says the connection is not live; a launch cannot be seen");
                    false
                }
                _ => true,
            },
        },
    };
    if let Some(held) = HELD.lock().unwrap_or_else(|p| p.into_inner()).as_mut() {
        let m = &mut held.machine;
        m.live = live;
        // Without a live subscription a launch could not be seen, and a
        // retry could double a join.
        m.retry = live && !m.no_publish && std::env::var_os(RETRY_VARIABLE).is_some();
    }
    println!("[deeplink] holding the link until the home page is interactive; it is then published once");
    Outcome::Published
}

fn release_held() {
    linker::game_activity::app_ready_set_sink(None);
    linker::game_activity::home_interactive_set_sink(None);
    WORK.fetch_and(!HELD_BIT, Ordering::AcqRel);
    take_launches();
}

/// One looper tick: devctl's bus verbs, then the held link.
pub fn tick() {
    let work = WORK.load(Ordering::Acquire);
    if work == 0 {
        return;
    }
    if work & DEV_BIT != 0 {
        serve_dev_requests();
    }
    if work & HELD_BIT != 0 {
        tick_held();
    }
}

fn tick_held() {
    let mut guard = HELD.lock().unwrap_or_else(|p| p.into_inner());
    let Some(held) = guard.as_mut() else { return };
    let mut engine = LiveEngine { publish_raw: held.publish_raw, cold_start_query: held.cold_start_query };
    // A second pass straight after a publish: the engine answers from inside
    // the publish call, so its Game.launch is already queued.
    for _ in 0..2 {
        let publishes = held.machine.publishes;
        let inputs = Inputs {
            home_up: HOME_UP.load(Ordering::Acquire),
            landings: LANDINGS.load(Ordering::Acquire),
            launches: take_launches(),
            joins: crate::engine_log::joins_seen(),
        };
        for line in held.machine.step(Instant::now(), &inputs, &mut engine) {
            println!("{line}");
        }
        if held.machine.done || held.machine.publishes == publishes {
            break;
        }
    }
    if held.machine.done {
        *guard = None;
        release_held();
    }
}

// ---------------------------------------------------------------- devctl

struct DevRequest {
    verb: String,
    arg: String,
    reply: mpsc::Sender<String>,
}

static DEV_QUEUE: Mutex<Vec<DevRequest>> = Mutex::new(Vec::new());
static LIVE: Mutex<Option<linker::Library>> = Mutex::new(None);
/// Payloads devctl's `bussub` sink has seen.
static DEV_BUS_PAYLOADS: AtomicU64 = AtomicU64::new(0);

/// Registers the loaded engine for devctl's `bussub`, `deeplink` and
/// `buslast`. Called just before the looper pump starts.
pub fn arm_live(lib: linker::Library) {
    *LIVE.lock().unwrap_or_else(|p| p.into_inner()) = Some(lib);
}

/// devctl's `bussub <id>`, `deeplink <url>` and `buslast <id>`, answered on
/// the looper thread. Blocks the socket thread for at most 5 s.
pub fn dev_request(verb: &str, arg: &str) -> String {
    if LIVE.lock().unwrap_or_else(|p| p.into_inner()).is_none() {
        return "err no engine armed yet".into();
    }
    let (reply, answer) = mpsc::channel();
    DEV_QUEUE.lock().unwrap_or_else(|p| p.into_inner()).push(DevRequest {
        verb: verb.to_string(),
        arg: arg.trim().to_string(),
        reply,
    });
    WORK.fetch_or(DEV_BIT, Ordering::AcqRel);
    answer.recv_timeout(Duration::from_secs(5)).unwrap_or_else(|_| "err the looper did not answer in 5 s".into())
}

fn serve_dev_requests() {
    WORK.fetch_and(!DEV_BIT, Ordering::AcqRel);
    let requests = std::mem::take(&mut *DEV_QUEUE.lock().unwrap_or_else(|p| p.into_inner()));
    let lib = *LIVE.lock().unwrap_or_else(|p| p.into_inner());
    for request in requests {
        let reply = match lib {
            None => "err no engine armed yet".into(),
            Some(lib) => dev_answer(lib, &request.verb, &request.arg),
        };
        let _ = request.reply.send(reply);
    }
}

fn dev_answer(lib: linker::Library, verb: &str, arg: &str) -> String {
    match verb {
        "bussub" => dev_bussub(arg, |id, sink| match lib.symbol(bus::DO_SUBSCRIBE_RAW) {
            None => None,
            Some(f) => Some(bus::subscribe(f, id, Some(sink))),
        }),
        "deeplink" => {
            let publish_raw = lib.symbol(bus::PUBLISH_RAW);
            let (reply, lines) = dev_deeplink(arg, |json| {
                publish_raw.map(|f| bus::publish(f, DETECT_URL, json))
            });
            for line in lines {
                println!("{line}");
            }
            reply
        }
        "buslast" => {
            if arg.is_empty() {
                return "err buslast <message id>".into();
            }
            let Some(f) = lib.symbol(bus::GET_LAST_RAW) else { return "err getLastRaw is not exported".into() };
            match linker::game_activity::call_static_string_ret_string(f, bus::MESSAGE_BUS_CLASS, arg) {
                Ok(payload) if payload.is_empty() => "ok none".into(),
                Ok(payload) => format!("ok {}", payload.replace(['\n', '\r'], " ")),
                Err(e) => {
                    println!("[deeplink] getLastRaw({arg}) failed: {e}");
                    "ok none".into()
                }
            }
        }
        other => format!("err unknown verb {other:?}"),
    }
}

/// `bussub`: `subscribe` is `None` when `doSubscribeRaw` is not exported. A
/// topic that carries clipboard text gets a listener that never prints the
/// payload, whatever [`PROBE_VARIABLE`] says.
fn dev_bussub(
    id: &str,
    subscribe: impl FnOnce(&str, bus::Sink) -> Option<Result<bus::Joined, String>>,
) -> String {
    if id.is_empty() {
        return "err bussub <message id>".into();
    }
    let sink: bus::Sink = if crate::android::clipboard::is_private_topic(id) {
        on_dev_private_payload
    } else {
        on_dev_bus_payload
    };
    match subscribe(id, sink) {
        None => "err doSubscribeRaw is not exported".into(),
        Some(Ok(_)) => "ok subscribed".into(),
        Some(Err(e)) => format!("err subscribe failed: {e}"),
    }
}

/// `deeplink`: the same checks and translation as `--join-url`, then one
/// publish. A refused link publishes nothing. The held link, its counters and
/// its verdict are not touched. Returns the reply and the lines to print.
fn dev_deeplink(
    arg: &str,
    publish: impl FnOnce(&str) -> Option<Result<(), String>>,
) -> (String, Vec<String>) {
    let url = match validate(arg) {
        Ok(url) => url,
        Err(e) => return (format!("err {e}"), Vec::new()),
    };
    let line = match publish(&detect_url_payload(url.delivered())) {
        None => return ("err not published".into(), vec!["[deeplink] MessageBus.publishRaw is not exported by this build".into()]),
        Some(Ok(())) => "[deeplink] (devctl) published Linking.detectURL".to_string(),
        Some(Err(e)) => return ("err not published".into(), vec![format!("[deeplink] (devctl) publishing Linking.detectURL failed: {e}")]),
    };
    ("ok published".into(), vec![format!("[deeplink] devctl: {}", describe(url.delivered())), line])
}

fn probe() -> bool {
    std::env::var_os(PROBE_VARIABLE).is_some()
}

/// `payload` with control characters written as `\u{..}`, so a payload is one
/// log line whatever it holds.
fn one_line(payload: &[u8]) -> String {
    String::from_utf8_lossy(payload).chars().flat_map(|c| {
        if c.is_control() { c.escape_unicode().collect::<Vec<_>>() } else { vec![c] }
    }).collect()
}

extern "C" fn on_dev_bus_payload(payload: *const c_char) {
    DEV_BUS_PAYLOADS.fetch_add(1, Ordering::AcqRel);
    // SAFETY: the bus hands a NUL-terminated string valid for the call, or null.
    match unsafe { bus::payload_bytes(payload) } {
        None => println!("[devctl bus] a null payload"),
        Some(bytes) if probe() => println!("[devctl bus] payload {}", one_line(bytes)),
        Some(bytes) => println!("[devctl bus] a payload of {} bytes ({PROBE_VARIABLE} shows it)", bytes.len()),
    }
}

/// devctl's listener on a clipboard topic: the size only, always.
extern "C" fn on_dev_private_payload(payload: *const c_char) {
    DEV_BUS_PAYLOADS.fetch_add(1, Ordering::AcqRel);
    // SAFETY: as in `on_dev_bus_payload`.
    match unsafe { bus::payload_bytes(payload) } {
        None => println!("[devctl bus] a null payload"),
        Some(bytes) => println!("[devctl bus] a payload of {} bytes (clipboard text is never shown)", bytes.len()),
    }
}

#[cfg(test)]
mod tests;
