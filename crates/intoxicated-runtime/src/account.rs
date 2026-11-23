//! Who is signed in: the identity behind the session, kept in the profile.
//!
//! The cookie alone signs nobody in. The engine's account router asks the host
//! which user is signed in (`NativeUserJavaInterface`, and `StartAppParams`
//! once at app start) and routes to the landing page when told user 0, before
//! it touches the network. It also keeps a copy of its own, set through
//! `nativeSetUserId`. So the identity is told three times, and the three must
//! agree:
//!
//! 1. the mirrors in `native/jni_values.cpp`, filled by [`restore`]
//!    before the engine library is even loaded, and again on every login;
//! 2. `StartAppParams`, which the native side fills from those mirrors when
//!    the app starts, which is why the mirrors are filled first;
//! 3. `nativeSetUserId`, through [`tell_engine`], after the app bridge (earlier
//!    it does nothing) and before the app shell starts.
//!
//! The identity arrives with the engine's `DID_LOG_IN` notification, which
//! also fires on a launch whose session was restored, so the store is
//! rewritten on every signed-in launch. `DID_LOG_OUT` erases it.
//!
//! **This is personal data.** Nothing here prints a user id, username, display
//! name or country code at any verbosity; lines carry byte lengths and paths.
//! [`Identity`]'s `Debug` follows the same rule.
//!
//! **The store** is `<profile>/identity`, through [`crate::secrets`]: one line
//! of compact JSON with keys in alphabetical order and a final LF, holding
//! `schema` 1, the seven identity fields and a fixed [`WARNING`]. Reading
//! accepts any JSON layout and key order; see [`Identity::from_store`].

use std::ffi::{c_char, c_void, CStr};
use std::sync::{Mutex, MutexGuard};

use intoxicated_linker_sys::game_activity as engine;
use serde_json::{json, Map, Value};

use crate::secrets::{self, Kind};

/// The text every stored identity carries, for whoever opens the file.
pub const WARNING: &str = "a live Roblox identity; treat it as personal data and keep it out of any bug report";

/// The only store schema this version reads or writes.
pub const SCHEMA: u64 = 1;

/// The switch that turns identity handling off for a run.
pub const SKIP_VARIABLE: &str = "INTOXICATED_SKIP_IDENTITY";

const SETTINGS_CLASS: &str = "com/roblox/engine/jni/NativeSettingsInterface";

/// A signed-in user.
#[derive(Clone, PartialEq, Eq)]
pub struct Identity {
    /// Never 0.
    pub user_id: i64,
    /// Never empty.
    pub username: String,
    pub display_name: String,
    pub membership_type: i64,
    pub is_under13: bool,
    pub has_subscription: bool,
    /// Stored, not published.
    pub country_code: String,
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Identity(username {} bytes)", self.username.len())
    }
}

impl Identity {
    /// The identity fields of a JSON object, by name, in any order.
    ///
    /// `userId` must be a non-zero integer and `username` a non-empty string;
    /// the other fields fall back to empty, 0 or false when missing or of the
    /// wrong type.
    fn from_fields(map: &Map<String, Value>) -> Option<Identity> {
        let user_id = map.get("userId")?.as_i64().filter(|&id| id != 0)?;
        let username = map.get("username")?.as_str().filter(|s| !s.is_empty())?.to_string();
        let text = |k: &str| map.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
        let flag = |k: &str| map.get(k).and_then(Value::as_bool).unwrap_or(false);
        Some(Identity {
            user_id,
            username,
            display_name: text("displayName"),
            membership_type: map.get("membershipType").and_then(Value::as_i64).unwrap_or(0),
            is_under13: flag("isUnder13"),
            has_subscription: flag("hasRobloxSubscription"),
            country_code: text("countryCode"),
        })
    }

    /// The payload of a login notification.
    pub fn from_login(payload: &str) -> Option<Identity> {
        match serde_json::from_str::<Value>(payload).ok()? {
            Value::Object(map) => Identity::from_fields(&map),
            _ => None,
        }
    }

    /// A stored identity. Anything but `schema` 1 (a future schema too) is
    /// nobody.
    pub fn from_store(text: &str) -> Option<Identity> {
        let Value::Object(map) = serde_json::from_str::<Value>(text).ok()? else { return None };
        if map.get("schema").and_then(Value::as_u64) != Some(SCHEMA) {
            return None;
        }
        Identity::from_fields(&map)
    }

    /// The store's text.
    pub fn to_store(&self) -> String {
        // `serde_json`'s map is ordered by key, which is what gives the file
        // its alphabetical order.
        let doc = json!({
            "countryCode": self.country_code,
            "displayName": self.display_name,
            "hasRobloxSubscription": self.has_subscription,
            "isUnder13": self.is_under13,
            "membershipType": self.membership_type,
            "schema": SCHEMA,
            "userId": self.user_id,
            "username": self.username,
            "warning": WARNING,
        });
        doc.to_string() + "\n"
    }

    fn publish(&self) {
        engine::identity_publish(
            self.user_id,
            &self.username,
            &self.display_name,
            self.membership_type,
            self.is_under13,
            self.has_subscription,
        );
    }
}

/// The identity this process believes is signed in.
static CURRENT: Mutex<Option<Identity>> = Mutex::new(None);

fn current() -> MutexGuard<'static, Option<Identity>> {
    CURRENT.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Whether this run handles identity at all. Read once.
pub fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os(SKIP_VARIABLE).is_none())
}

/// The signed-in user's id, or 0 for nobody.
pub fn current_user_id() -> i64 {
    current().as_ref().map_or(0, |id| id.user_id)
}

/// Install the login and logout sinks. Always, even with identity off: the
/// sinks do nothing then, and the engine's callback still resolves.
pub fn listen() {
    engine::identity_set_sinks(on_login, on_logout);
}

/// Fill the mirrors from the store. Touches no engine symbol, so it runs
/// before the engine library is loaded.
pub fn restore() {
    if !enabled() {
        println!("  [identity] off ({SKIP_VARIABLE}); the mirrors will report nobody");
        return;
    }
    let dir = crate::profile::active();
    let store = secrets::active();
    let Some(identity) = secrets::load(store, &dir, Kind::Identity).as_deref().and_then(Identity::from_store) else {
        return;
    };
    identity.publish();
    let bytes = identity.username.len();
    *current() = Some(identity);
    println!(
        "  [identity] restored a signed-in user from {} (username {bytes} bytes)",
        secrets::where_kept(store, &dir, Kind::Identity)
    );
}

/// Give the engine its own copy of the signed-in user, through
/// `nativeSetUserId`. Once, after the app bridge and the cookie restore and
/// before the app shell starts; a login later in the run does not call it
/// again.
pub fn tell_engine(set_user_id: Option<*mut c_void>) {
    if !enabled() {
        return;
    }
    let Some(f) = set_user_id else {
        println!("  [identity] nativeSetUserId not exported; the engine will not know who is signed in");
        return;
    };
    let id = current_user_id();
    if id == 0 {
        return;
    }
    match engine::call_static_strings(f, SETTINGS_CLASS, &[&id.to_string()]) {
        Ok(()) => println!("  [identity] the engine has been told which user is signed in"),
        Err(e) => eprintln!("[identity] nativeSetUserId failed: {e}"),
    }
}

/// A login: make it current, publish it, and save it, every time.
fn signed_in(payload: &str) {
    let Some(identity) = Identity::from_login(payload) else {
        println!("  [identity] a login notification of {} bytes did not carry a usable identity", payload.len());
        return;
    };
    identity.publish();
    let body = identity.to_store();
    let bytes = identity.username.len();
    *current() = Some(identity);
    let dir = crate::profile::active();
    let store = secrets::active();
    match secrets::save(store, &dir, Kind::Identity, &body) {
        Ok(()) => println!(
            "  [identity] signed in; saved to {} (username {bytes} bytes)",
            secrets::where_kept(store, &dir, Kind::Identity)
        ),
        Err(e) => eprintln!("[identity] the identity was not saved: {e}"),
    }
}

/// A logout: forget, clear the mirrors, erase the store.
fn signed_out() {
    let had = current().take().is_some();
    engine::identity_clear();
    match secrets::erase(secrets::active(), &crate::profile::active(), Kind::Identity) {
        Ok(()) if had => println!("  [identity] signed out; the saved identity is gone"),
        Ok(()) => {}
        Err(e) => eprintln!("[identity] could not remove the saved identity: {e}"),
    }
}

/// `DID_LOG_IN`, `DID_SIGN_UP` and `DID_SWITCH_ACCOUNT`, on the engine's
/// thread. Calls nothing in the engine.
extern "C" fn on_login(payload: *const c_char) {
    if !enabled() || payload.is_null() {
        return;
    }
    // SAFETY: the C side passes a NUL-terminated string valid for this call.
    let payload = unsafe { CStr::from_ptr(payload) }.to_string_lossy();
    signed_in(&payload);
}

/// `DID_LOG_OUT` and `LUA_UNAUTHORIZED_LOG_OUT`.
extern "C" fn on_logout() {
    if enabled() {
        signed_out();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake() -> Identity {
        Identity {
            user_id: 1234567890,
            username: "FakeUser".into(),
            display_name: "FakeName".into(),
            membership_type: 4,
            is_under13: false,
            has_subscription: true,
            country_code: "ZZ".into(),
        }
    }

    #[test]
    fn the_store_round_trips_in_todays_layout() {
        let text = fake().to_store();
        assert!(text.ends_with("}\n"));
        assert_eq!(text.matches('\n').count(), 1, "one line");
        assert!(!text.contains(": ") && !text.contains(", "), "compact");
        let keys: Vec<&str> = [
            "countryCode",
            "displayName",
            "hasRobloxSubscription",
            "isUnder13",
            "membershipType",
            "schema",
            "userId",
            "username",
            "warning",
        ]
        .to_vec();
        let positions: Vec<usize> = keys.iter().map(|k| text.find(&format!("\"{k}\":")).unwrap()).collect();
        assert!(positions.windows(2).all(|w| w[0] < w[1]), "alphabetical: {positions:?}");
        assert!(text.contains(&format!("\"warning\":\"{WARNING}\"")));
        assert!(text.contains("\"schema\":1"));
        assert_eq!(Identity::from_store(&text), Some(fake()));
    }

    #[test]
    fn todays_file_reads() {
        // The structure of the file on disk today, with fake values.
        let text = format!(
            "{{\"countryCode\":\"ZZ\",\"displayName\":\"FakeName\",\"hasRobloxSubscription\":false,\
             \"isUnder13\":false,\"membershipType\":0,\"schema\":1,\"userId\":1234567890,\
             \"username\":\"FakeUser\",\"warning\":\"{WARNING}\"}}\n"
        );
        let id = Identity::from_store(&text).expect("an identity");
        assert_eq!((id.user_id, id.username.as_str(), id.display_name.as_str()), (1234567890, "FakeUser", "FakeName"));
        assert_eq!(id.to_store(), text, "rewriting an unchanged identity reproduces the file");
    }

    #[test]
    fn any_layout_and_order_reads() {
        let pretty = "{\n  \"username\": \"FakeUser\",\n  \"userId\": 77,\n  \"schema\": 1\n}\n";
        let id = Identity::from_store(pretty).unwrap();
        assert_eq!(id.user_id, 77);
        assert_eq!(id.display_name, "");
        assert_eq!(id.country_code, "");
        assert_eq!(id.membership_type, 0);
        assert!(!id.is_under13 && !id.has_subscription);
    }

    #[test]
    fn what_is_nobody() {
        let ok = r#"{"schema":1,"userId":5,"username":"u"}"#;
        assert!(Identity::from_store(ok).is_some());
        for bad in [
            r#"{"userId":5,"username":"u"}"#,
            r#"{"schema":2,"userId":5,"username":"u"}"#,
            r#"{"schema":"1","userId":5,"username":"u"}"#,
            r#"{"schema":1.0,"userId":5,"username":"u"}"#,
            r#"{"schema":1,"userId":0,"username":"u"}"#,
            r#"{"schema":1,"userId":"5","username":"u"}"#,
            r#"{"schema":1,"userId":5,"username":""}"#,
            r#"{"schema":1,"userId":5}"#,
            r#"[1]"#,
            "not json",
            "",
        ] {
            assert!(Identity::from_store(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn wrong_types_fall_back_rather_than_refuse() {
        let id = Identity::from_store(
            r#"{"schema":1,"userId":5,"username":"u","displayName":7,"isUnder13":"yes","membershipType":"x","countryCode":null}"#,
        )
        .unwrap();
        assert_eq!((id.display_name.as_str(), id.is_under13, id.membership_type, id.country_code.as_str()), ("", false, 0, ""));
    }

    #[test]
    fn a_login_payload_reads_without_a_schema() {
        let payload = r#"{"username":"FakeUser","membershipType":4,"isUnder13":false,"hasRobloxSubscription":true,"countryCode":"ZZ","userId":1234567890,"displayName":"FakeName"}"#;
        assert_eq!(Identity::from_login(payload), Some(fake()));
        assert!(Identity::from_login(r#"{"userId":0,"username":"x"}"#).is_none());
        assert!(Identity::from_login("").is_none());
    }

    #[test]
    fn debug_shows_no_personal_data() {
        let shown = format!("{:?}", fake());
        assert_eq!(shown, "Identity(username 8 bytes)");
    }
}
