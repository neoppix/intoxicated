//! The engine's per-account "secure values", kept in the profile.
//!
//! The engine asks the platform, through the djinni interface
//! `IPlatformLocalStorageHandler`, to hold small string values keyed by an
//! account id and a name it picks. `native/secure_values.cpp` is that
//! interface's Java side; this module is the store behind it. It is unrelated
//! to the engine's content store (`RbxStorage`) and to `LocalStorageManager`.
//!
//! **The file** is `<profile>/local-storage-secrets.json`: one JSON object
//! whose keys are account ids as decimal strings (`"0"` is nobody) and whose
//! values are objects of key name to string value. Compact, no trailing
//! newline, `0600`, written beside the file and renamed in after an fsync.
//! Every write or delete rereads the file, applies the one change and writes
//! it back. A missing, unreadable or invalid file reads as empty, and the next
//! write replaces it. Not [`crate::secrets`]: that holds one document per kind,
//! and this is an open set of values.
//!
//! **Size.** A value is at most [`MAX_VALUE_BYTES`] bytes, written or read:
//! anything that can be stored reads back whole, and a longer value (only
//! possible in a file edited by hand) reads as absent rather than cut short.
//!
//! **The current account** is decided on the first call that needs it: the
//! signed-in user if there is one ([`crate::account::current_user_id`]),
//! else 0. After that only `setCurrentUser` moves it; a login later in the
//! run does not.
//!
//! **Printing.** Key names may be printed. Values and account ids never are.

use std::collections::BTreeSet;
use std::ffi::{c_char, c_int, c_longlong, c_void};
use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

use serde_json::{Map, Value};

/// The store's file name in the profile.
pub const FILE_NAME: &str = "local-storage-secrets.json";

/// The largest value stored or returned, in bytes.
pub const MAX_VALUE_BYTES: usize = 8192;

/// What a read found.
#[derive(Debug, PartialEq, Eq)]
pub enum Lookup {
    Found(String),
    Absent,
    /// A stored value longer than [`MAX_VALUE_BYTES`], with its length.
    TooLarge(usize),
}

/// The file, and what can be done to it.
#[derive(Clone, Debug)]
pub struct Store {
    path: PathBuf,
}

impl Store {
    pub fn at(path: PathBuf) -> Store {
        Store { path }
    }

    /// The store in the active profile.
    pub fn in_profile() -> Store {
        Store::at(crate::profile::active().join(FILE_NAME))
    }

    fn read(&self) -> Map<String, Value> {
        std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|text| match serde_json::from_str(&text) {
                Ok(Value::Object(map)) => Some(map),
                _ => None,
            })
            .unwrap_or_default()
    }

    fn write(&self, doc: &Map<String, Value>) -> std::io::Result<()> {
        let body = serde_json::to_string(doc).map_err(std::io::Error::other)?;
        let mut tmp_name = self.path.file_name().unwrap_or_default().to_os_string();
        tmp_name.push(".new");
        let tmp = self.path.with_file_name(tmp_name);
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
        f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        f.write_all(body.as_bytes())?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, &self.path)
    }

    /// The value of `key` for `user`.
    pub fn get(&self, user: i64, key: &str) -> Lookup {
        match self.read().get(&user.to_string()).and_then(|values| values.get(key)) {
            Some(Value::String(s)) if s.len() > MAX_VALUE_BYTES => Lookup::TooLarge(s.len()),
            Some(Value::String(s)) => Lookup::Found(s.clone()),
            _ => Lookup::Absent,
        }
    }

    /// Store `value` as `key` for `user`. Refuses a value over the limit.
    pub fn set(&self, user: i64, key: &str, value: &str) -> Result<(), String> {
        if value.len() > MAX_VALUE_BYTES {
            return Err(format!("{} bytes is over the {MAX_VALUE_BYTES}-byte limit", value.len()));
        }
        let mut doc = self.read();
        let values = doc.entry(user.to_string()).or_insert_with(|| Value::Object(Map::new()));
        if !values.is_object() {
            *values = Value::Object(Map::new());
        }
        values.as_object_mut().expect("made an object above").insert(key.to_string(), Value::String(value.into()));
        self.write(&doc).map_err(|e| e.to_string())
    }

    /// Remove `key` for `user`. Absent is success; the file is rewritten
    /// either way, and an account left with no values is dropped.
    pub fn delete(&self, user: i64, key: &str) -> Result<(), String> {
        let mut doc = self.read();
        let id = user.to_string();
        if let Some(values) = doc.get_mut(&id).and_then(Value::as_object_mut) {
            values.remove(key);
            if values.is_empty() {
                doc.remove(&id);
            }
        }
        self.write(&doc).map_err(|e| e.to_string())
    }

    /// Remove every value of `user`.
    pub fn delete_user(&self, user: i64) -> Result<(), String> {
        let mut doc = self.read();
        doc.remove(&user.to_string());
        self.write(&doc).map_err(|e| e.to_string())
    }
}

/// The accounts the handler has been told about.
#[derive(Debug, Default)]
pub struct Accounts {
    current: Option<i64>,
    known: BTreeSet<i64>,
}

impl Accounts {
    pub const fn new() -> Accounts {
        Accounts { current: None, known: BTreeSet::new() }
    }

    /// The current account, deciding it from `signed_in` the first time.
    pub fn current(&mut self, signed_in: impl FnOnce() -> i64) -> i64 {
        if let Some(id) = self.current {
            return id;
        }
        let id = signed_in();
        if id != 0 {
            self.known.insert(id);
        }
        self.current = Some(id);
        id
    }

    pub fn set_current(&mut self, id: i64) {
        self.current = Some(id);
        self.known.insert(id);
    }

    pub fn forget(&mut self, id: i64) {
        self.known.remove(&id);
    }

    pub fn known(&self) -> usize {
        self.known.len()
    }
}

static ACCOUNTS: Mutex<Accounts> = Mutex::new(Accounts::new());
/// One read-change-write at a time: the engine calls from its own threads.
static FILE: Mutex<()> = Mutex::new(());

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn current_user() -> i64 {
    lock(&ACCOUNTS).current(crate::account::current_user_id)
}

/// `(pointer, length)` from C as text, or `None` when it is not UTF-8.
///
/// SAFETY: the caller guarantees `len` readable bytes at `ptr`.
unsafe fn text<'a>(ptr: *const c_char, len: usize) -> Option<&'a str> {
    if ptr.is_null() {
        return None;
    }
    std::str::from_utf8(std::slice::from_raw_parts(ptr.cast::<u8>(), len)).ok()
}

/// A key name, made printable whatever its bytes.
///
/// SAFETY: as for [`text`].
unsafe fn shown_key(ptr: *const c_char, len: usize) -> String {
    if ptr.is_null() {
        return String::new();
    }
    String::from_utf8_lossy(std::slice::from_raw_parts(ptr.cast::<u8>(), len)).into_owned()
}

/// The table `native/secure_values.cpp` calls through. Must match
/// `IntoxSecureValueOps` there field for field.
#[repr(C)]
pub struct Ops {
    current_user: extern "C" fn() -> c_longlong,
    set_current_user: extern "C" fn(c_longlong),
    known_users: extern "C" fn() -> c_longlong,
    get: extern "C" fn(
        c_longlong,
        *const c_char,
        usize,
        extern "C" fn(*mut c_void, *const c_char, usize),
        *mut c_void,
    ) -> c_int,
    set: extern "C" fn(c_longlong, *const c_char, usize, *const c_char, usize) -> c_int,
    remove: extern "C" fn(c_longlong, *const c_char, usize) -> c_int,
    remove_user: extern "C" fn(c_longlong) -> c_int,
}

static OPS: Ops = Ops {
    current_user: op_current_user,
    set_current_user: op_set_current_user,
    known_users: op_known_users,
    get: op_get,
    set: op_set,
    remove: op_remove,
    remove_user: op_remove_user,
};

extern "C" fn op_current_user() -> c_longlong {
    current_user()
}

extern "C" fn op_set_current_user(id: c_longlong) {
    lock(&ACCOUNTS).set_current(id);
}

extern "C" fn op_known_users() -> c_longlong {
    // Decide the current account first, so a seeded user is counted.
    let _ = current_user();
    lock(&ACCOUNTS).known() as c_longlong
}

/// 1 and one call of `receive` with the value when found, else 0.
extern "C" fn op_get(
    user: c_longlong,
    key: *const c_char,
    key_len: usize,
    receive: extern "C" fn(*mut c_void, *const c_char, usize),
    ctx: *mut c_void,
) -> c_int {
    // SAFETY: the C side passes `key_len` bytes at `key`.
    let Some(name) = (unsafe { text(key, key_len) }) else { return 0 };
    let found = {
        let _file = lock(&FILE);
        Store::in_profile().get(user, name)
    };
    match found {
        Lookup::Found(value) => {
            receive(ctx, value.as_ptr().cast(), value.len());
            1
        }
        Lookup::Absent => 0,
        Lookup::TooLarge(n) => {
            println!(
                "  [local-storage] {name}: {n} bytes is over the {MAX_VALUE_BYTES}-byte limit; treated as absent \
                 rather than truncated"
            );
            0
        }
    }
}

/// 1 when the value was saved.
extern "C" fn op_set(user: c_longlong, key: *const c_char, key_len: usize, value: *const c_char, value_len: usize) -> c_int {
    // SAFETY: the C side passes the two lengths it read the strings with.
    let shown = unsafe { shown_key(key, key_len) };
    let Some(name) = (unsafe { text(key, key_len) }) else {
        println!("  [local-storage] {shown}: the key is not UTF-8; refused rather than stored");
        return 0;
    };
    // SAFETY: as above.
    let Some(value) = (unsafe { text(value, value_len) }) else {
        println!("  [local-storage] {name}: value is not UTF-8; refused rather than stored");
        return 0;
    };
    if value.len() > MAX_VALUE_BYTES {
        println!(
            "  [local-storage] {name}: {} bytes is over the {MAX_VALUE_BYTES}-byte limit; refused rather than \
             stored",
            value.len()
        );
        return 0;
    }
    let saved = {
        let _file = lock(&FILE);
        Store::in_profile().set(user, name, value)
    };
    match saved {
        Ok(()) => 1,
        Err(e) => {
            println!("  [local-storage] {name}: not saved ({e})");
            0
        }
    }
}

/// 1 when the file was saved.
extern "C" fn op_remove(user: c_longlong, key: *const c_char, key_len: usize) -> c_int {
    // SAFETY: the C side passes `key_len` bytes at `key`.
    let Some(name) = (unsafe { text(key, key_len) }) else { return 0 };
    let saved = {
        let _file = lock(&FILE);
        Store::in_profile().delete(user, name)
    };
    match saved {
        Ok(()) => 1,
        Err(e) => {
            println!("  [local-storage] {name}: not saved ({e})");
            0
        }
    }
}

/// 1 when the file was saved. The account leaves the known set either way.
extern "C" fn op_remove_user(user: c_longlong) -> c_int {
    lock(&ACCOUNTS).forget(user);
    let saved = {
        let _file = lock(&FILE);
        Store::in_profile().delete_user(user)
    };
    match saved {
        Ok(()) => 1,
        Err(e) => {
            println!("  [local-storage] account values: not saved ({e})");
            0
        }
    }
}

extern "C" {
    fn intox_secure_values_hand_over(native: *mut c_void, ops: *const Ops, err: *mut c_char, err_len: usize) -> c_int;
}

/// `ILocalStorageHandlerCore.setPlatformImpl(IPlatformLocalStorageHandler)`:
/// give the engine the handler backed by this store. Once per launch, after
/// the cookie handler is registered and before the init params are set.
pub fn hand_over(set_platform_impl: *mut c_void) -> Result<(), String> {
    let mut err = [0u8; 512];
    // SAFETY: `OPS` is static, `err` outlives the call, and the C side checks
    // the native for null.
    let rc = unsafe { intox_secure_values_hand_over(set_platform_impl, &OPS, err.as_mut_ptr().cast(), err.len()) };
    if rc == 0 {
        Ok(())
    } else {
        let end = err.iter().position(|&b| b == 0).unwrap_or(err.len());
        Err(String::from_utf8_lossy(&err[..end]).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> Store {
        let dir = std::env::temp_dir().join(format!("intox-secure-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Store::at(dir.join(FILE_NAME))
    }

    #[test]
    fn todays_empty_file_reads_as_empty() {
        let store = scratch("empty");
        std::fs::write(&store.path, "{}").unwrap();
        assert_eq!(store.get(0, "anything"), Lookup::Absent);
        assert_eq!(store.get(1234567890, "anything"), Lookup::Absent);
        store.delete(0, "anything").unwrap();
        assert_eq!(std::fs::read_to_string(&store.path).unwrap(), "{}", "a delete of nothing keeps the file as it is");
    }

    #[test]
    fn values_round_trip_per_account_in_the_old_layout() {
        let store = scratch("roundtrip");
        store.set(0, "deviceKey", "fake-value-0").unwrap();
        store.set(1234567890, "token", "fake \"quoted\" value").unwrap();
        store.set(1234567890, "other", "").unwrap();
        assert_eq!(store.get(0, "deviceKey"), Lookup::Found("fake-value-0".into()));
        assert_eq!(store.get(1234567890, "token"), Lookup::Found("fake \"quoted\" value".into()));
        assert_eq!(store.get(1234567890, "other"), Lookup::Found(String::new()), "an empty value is a value");
        assert_eq!(store.get(1, "token"), Lookup::Absent);

        let text = std::fs::read_to_string(&store.path).unwrap();
        assert!(!text.ends_with('\n') && !text.contains(": "), "compact, no trailing newline: {text}");
        let doc: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(doc["0"]["deviceKey"], "fake-value-0");
        assert_eq!(doc["1234567890"]["other"], "");
        let mode = std::fs::metadata(&store.path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert!(!store.path.with_file_name(format!("{FILE_NAME}.new")).exists());

        // A file written by the old store, read as it is.
        std::fs::write(&store.path, r#"{"-5":{"k":"v"},"7":{"a":"b","n":3}}"#).unwrap();
        assert_eq!(store.get(-5, "k"), Lookup::Found("v".into()));
        assert_eq!(store.get(7, "n"), Lookup::Absent, "a value that is not a string is not a value");
    }

    #[test]
    fn deletes() {
        let store = scratch("delete");
        store.set(3, "a", "1").unwrap();
        store.set(3, "b", "2").unwrap();
        store.set(4, "a", "1").unwrap();
        store.delete(3, "a").unwrap();
        assert_eq!(store.get(3, "a"), Lookup::Absent);
        assert_eq!(store.get(3, "b"), Lookup::Found("2".into()));
        store.delete(3, "b").unwrap();
        store.delete_user(4).unwrap();
        assert_eq!(std::fs::read_to_string(&store.path).unwrap(), "{}");
    }

    #[test]
    fn exactly_8192_bytes_is_written_and_read_back() {
        let store = scratch("limit");
        let at_limit = "v".repeat(MAX_VALUE_BYTES);
        store.set(0, "big", &at_limit).unwrap();
        assert_eq!(store.get(0, "big"), Lookup::Found(at_limit.clone()));

        // One byte more is refused on the way in ...
        let over = "v".repeat(MAX_VALUE_BYTES + 1);
        assert!(store.set(0, "big", &over).is_err());
        assert_eq!(store.get(0, "big"), Lookup::Found(at_limit), "a refused write leaves the old value");

        // ... and a longer value already in the file reads as absent, never
        // cut short.
        std::fs::write(&store.path, format!("{{\"0\":{{\"big\":\"{over}\"}}}}")).unwrap();
        assert_eq!(store.get(0, "big"), Lookup::TooLarge(MAX_VALUE_BYTES + 1));

        // Multi-byte text counts bytes, not characters.
        let wide = "\u{e9}".repeat(MAX_VALUE_BYTES / 2);
        assert_eq!(wide.len(), MAX_VALUE_BYTES);
        store.set(0, "wide", &wide).unwrap();
        assert_eq!(store.get(0, "wide"), Lookup::Found(wide));
    }

    #[test]
    fn an_invalid_file_is_empty_and_is_replaced() {
        let store = scratch("invalid");
        std::fs::write(&store.path, "not json").unwrap();
        assert_eq!(store.get(0, "k"), Lookup::Absent);
        store.set(0, "k", "v").unwrap();
        assert_eq!(std::fs::read_to_string(&store.path).unwrap(), r#"{"0":{"k":"v"}}"#);
        std::fs::write(&store.path, "[]").unwrap();
        assert_eq!(store.get(0, "k"), Lookup::Absent);
    }

    #[test]
    fn the_current_account_is_decided_once() {
        let mut a = Accounts::new();
        assert_eq!(a.current(|| 42), 42);
        assert_eq!(a.known(), 1);
        assert_eq!(a.current(|| panic!("decided already")), 42);
        a.set_current(7);
        assert_eq!(a.current(|| 0), 7);
        assert_eq!(a.known(), 2);
        a.forget(42);
        assert_eq!(a.known(), 1);

        let mut nobody = Accounts::new();
        assert_eq!(nobody.current(|| 0), 0);
        assert_eq!(nobody.known(), 0, "nobody is not an account");
    }

    extern "C" fn collect(ctx: *mut c_void, bytes: *const c_char, len: usize) {
        // SAFETY: `ctx` is the `Vec` the test passed; `bytes` holds `len` bytes.
        unsafe {
            let out = &mut *ctx.cast::<Vec<u8>>();
            out.extend_from_slice(std::slice::from_raw_parts(bytes.cast::<u8>(), len));
        }
    }

    #[test]
    fn the_c_table_refuses_text_that_is_not_utf8() {
        let bad = [0xffu8, 0xfe];
        let key = b"k";
        assert_eq!(op_set(0, key.as_ptr().cast(), 1, bad.as_ptr().cast(), bad.len()), 0);
        assert_eq!(op_set(0, bad.as_ptr().cast(), bad.len(), key.as_ptr().cast(), 1), 0);
        let mut out: Vec<u8> = Vec::new();
        assert_eq!(op_get(0, bad.as_ptr().cast(), bad.len(), collect, (&mut out as *mut Vec<u8>).cast()), 0);
        assert!(out.is_empty());
    }
}
