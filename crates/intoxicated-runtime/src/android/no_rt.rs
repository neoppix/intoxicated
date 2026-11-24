//! What stands in for `android::rt` in a client built without the `rt`
//! feature (the installer's ray tracing question answered no, or
//! `install.sh --no-rt`): the entry points the Vulkan layer, the effects and
//! devctl call, none of the ray tracing.
//!
//! Nothing of the capture, the hierarchy or the traced reflections and
//! light is built, nor their SPIR-V. `INTOXICATED_RT=1` is ignored, said once
//! in the log; devctl `rt` answers "not built"; the layer's hooks find
//! nothing to capture, so they forward every call as they would with the
//! switch off.

use std::ffi::c_void;
use std::sync::atomic::AtomicBool;
use std::sync::OnceLock;

use super::vk_layer::passes::Pass;

/// The switch the full module reads.
pub const ENV: &str = "INTOXICATED_RT";

/// How a user adds it, for the log and devctl.
const HOW_TO_ADD: &str = "run install.sh with --effects --rt (intoxicated --update --rt) to add it";

/// The trace's switch while the game runs; never on here.
pub static ON: AtomicBool = AtomicBool::new(false);

/// The log line for a switch set to `value`, or `None` when it asks for
/// nothing.
fn ignored_line(value: Option<&str>) -> Option<String> {
    (value.map(str::trim) == Some("1"))
        .then(|| format!("[android] vulkan: {ENV}=1 ignored: built without the ray tracing; {HOW_TO_ADD}"))
}

/// Read the switch once and say so when it asks for the ray tracing. Never
/// on.
pub(crate) fn requested() -> bool {
    static SAID: OnceLock<()> = OnceLock::new();
    SAID.get_or_init(|| {
        if let Some(line) = ignored_line(std::env::var(ENV).ok().as_deref()) {
            println!("{line}");
        }
    });
    false
}

/// devctl `rt`: not built.
pub fn control(_args: &[&str]) -> Result<String, String> {
    Err(format!("the ray tracing is not built in this client; {HOW_TO_ADD}"))
}

/// What the full module enables on a device; nothing here.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Enabled;

/// Never made: [`raise_api_version`] always answers `None`.
pub(crate) struct RaisedInstanceInfo {
    never: std::convert::Infallible,
}

impl RaisedInstanceInfo {
    pub(crate) fn as_ptr(&self) -> *const c_void {
        match self.never {}
    }
}

/// Never made: [`extend_device`] always answers `None`.
pub(crate) struct ExtendedDeviceInfo {
    pub(crate) enabled: Enabled,
    never: std::convert::Infallible,
}

impl ExtendedDeviceInfo {
    pub(crate) fn as_ptr(&self) -> *const c_void {
        match self.never {}
    }
}

/// # Safety
/// Nothing is read.
pub(crate) unsafe fn raise_api_version(_info: *const c_void) -> Option<RaisedInstanceInfo> {
    None
}

/// # Safety
/// Nothing is read.
pub(crate) unsafe fn extend_device(_pd: *mut c_void, _info: *const c_void) -> Option<ExtendedDeviceInfo> {
    None
}

pub(crate) fn device_created(_device: *mut c_void, _enabled: Enabled) {}

pub(crate) fn device_destroying(_device: *mut c_void) {}

/// The capture's hooks: never wanted, so the layer forwards every call.
pub(crate) mod capture {
    use super::Pass;
    use std::ffi::c_void;

    /// Never; the first call says once that `INTOXICATED_RT=1` is
    /// ignored (see [`super::requested`]).
    pub(crate) fn wanted() -> bool {
        super::requested()
    }

    pub(crate) fn hook(_name: &[u8], next: *mut c_void) -> *mut c_void {
        next
    }

    // Called from the effects' own hooks (`passes::compute_state`), which
    // only a client with them has.
    #[cfg(feature = "shaders")]
    pub(crate) fn cmd_begun(_cmd: *mut c_void) {}

    pub(crate) fn pass_beginning(_pass: &Pass) {}

    #[cfg(feature = "shaders")]
    pub(crate) fn binding(_cmd: *mut c_void, _bind_point: u32) {}

    #[cfg(feature = "shaders")]
    pub(crate) fn bound(_cmd: *mut c_void, _bind_point: u32, _pipeline: u64) {}

    pub(crate) fn interrupted(_cmd: *mut c_void) {}

    pub(crate) fn pass_ending(_cmd: *mut c_void) {}

    pub(crate) fn pass_ended(_cmd: *mut c_void) {}

    pub(crate) fn drew(_cmd: *mut c_void, _kind: &'static str, _count: u32, _instances: u32) {}

    pub(crate) fn frame_end(_presents: u64) {}

    pub(crate) fn device_destroying(_device: *mut c_void) {}
}

/// The trace the reflections would hand their work to: never on.
#[cfg(feature = "shaders")]
pub(crate) mod trace {
    use std::ffi::c_void;

    pub(crate) fn is_on() -> bool {
        false
    }

    pub(crate) fn shade_flags() -> u32 {
        0
    }

    pub(crate) fn view_flags() -> u32 {
        0
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record(
        _cmd: *mut c_void,
        _device: usize,
        _ssr_set_layout: u64,
        _ssr_set: u64,
        _params: &super::super::ssr::Params,
        _extent: [u32; 2],
        _period_ns: f64,
        _mask: u64,
    ) -> Result<bool, String> {
        Ok(false)
    }

    pub(crate) fn failed(_why: &str) {}

    pub(crate) fn record_gi_apply(
        _cmd: *mut c_void,
        _device: usize,
        _ssr_set: u64,
        _params: &super::super::ssr::Params,
        _extent: [u32; 2],
    ) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_switch_is_ignored_with_one_line() {
        assert_eq!(ignored_line(None), None);
        assert_eq!(ignored_line(Some("0")), None);
        assert!(ignored_line(Some(" 1 ")).is_some_and(|l| l.contains("INTOXICATED_RT=1 ignored") && l.contains("--rt")));
        assert!(!requested());
        assert!(control(&["stats"]).is_err_and(|e| e.contains("not built")));
    }
}
