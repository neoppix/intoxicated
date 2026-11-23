//! The process JavaVM: libjnivm standing in for ART.
//!
//! There is one VM per process, created by [`create_vm`], owned by the C side
//! (`native/java_vm.cpp`) and never destroyed. Every [`crate::game_activity`]
//! call that reaches the engine needs it to exist first and reports "no
//! JavaVM" otherwise.

use std::ffi::c_void;

use crate::call::{c_text, with_message};
use crate::ffi;

/// Creates the process's JavaVM and registers Intoxicated's Java classes into
/// it, returning the `JavaVM*` to hand to the engine.
///
/// `None` when a VM already exists. The check and the creation are not
/// atomic, so this belongs on one thread.
pub fn create_vm() -> Option<*mut c_void> {
    // SAFETY: no arguments; the C side owns what it creates.
    let vm = unsafe { ffi::intoxicated_jni_create_vm() };
    (!vm.is_null()).then_some(vm)
}

/// Calls the engine's `JNI_OnLoad` with the process VM and returns what it
/// returned, normally `0x00010006` (JNI 1.6).
///
/// The shim catches anything the engine throws, so a C++ exception arrives
/// here as an error rather than as `std::terminate`.
pub fn call_on_load(on_load: *mut c_void) -> Result<i32, String> {
    if on_load.is_null() {
        return Err("JNI_OnLoad was not called: its address is null".to_string());
    }
    // SAFETY: `on_load` is the engine's exported JNI_OnLoad; the C side
    // checks for a VM before calling it.
    let (rc, message) = with_message(|err, len| unsafe { ffi::intoxicated_jni_call_onload(on_load, err, len) });
    match rc {
        // The shim uses -1 for "no VM" and passes the engine's own return
        // value through unchanged, so a JNI_OnLoad that returned -1 (JNI_ERR,
        // its documented failure value) arrives looking the same. Both are
        // failures; the null case is caught above, and the message names the
        // two that remain rather than guessing between them.
        -1 => Err("JNI_OnLoad failed: no JavaVM exists yet, or it returned JNI_ERR".to_string()),
        -2 => Err(format!("JNI_OnLoad threw a C++ exception: {}", message.text())),
        -3 => Err(format!("JNI_OnLoad threw: {}", message.text())),
        version => Ok(version),
    }
}

/// Writes libjnivm's stub dump of every Java class and member the engine has
/// reached for so far to `path`.
pub fn dump_classes(path: &str) -> Result<(), String> {
    let path = c_text(path)?;
    // SAFETY: `path` outlives the call.
    match unsafe { ffi::intoxicated_jni_dump_classes(path.as_ptr()) } {
        0 => Ok(()),
        -1 => Err("no JavaVM exists yet, so there are no classes to dump".to_string()),
        -2 => Err("libjnivm was built without JNI_DEBUG, which keeps the class records".to_string()),
        rc => Err(format!("the class dump failed with code {rc}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // No test here creates a VM, so these see the state every process starts
    // in, before `create_vm`.

    #[test]
    fn a_null_on_load_is_refused_in_rust() {
        let e = call_on_load(std::ptr::null_mut()).unwrap_err();
        assert!(e.contains("null"), "{e}");
    }

    #[test]
    fn dumping_before_the_vm_exists_says_so() {
        let e = dump_classes("/nonexistent/intoxicated-class-dump").unwrap_err();
        assert!(e.contains("no JavaVM"), "{e}");
        assert!(dump_classes("a\0b").unwrap_err().contains("nul byte"));
    }
}
