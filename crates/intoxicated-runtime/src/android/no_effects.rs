//! What stands in for `android::ssr` in a client built without the `shaders`
//! feature (`install.sh --no-effects`, or the visual effects declined at
//! install): the same entry points, none of the effects.
//!
//! Nothing of the reflections, the depth of field, the bloom, the lens or
//! the sharpening is built, nor their SPIR-V. Their switches
//! (`INTOXICATED_SSR`, `_DOF`, `_BLOOM`, `_LENS`, `_SHARPEN` and
//! `_SHADERS_MODE`) are ignored, said once in the log when one asks for
//! something, and so is the effects hotkey (`INTOXICATED_EFFECTS_HOTKEY`),
//! whose key then goes to Roblox; their devctl verbs answer "not built"; `--print-shader-looks`
//! and `--print-ssr-looks` print [`UNAVAILABLE_JSON`], which Intoxicated
//! Settings reads to leave the Shaders page out. The pass hooks never watch
//! for the effects (`WATCH_POST` is never set), so the calls the layer makes
//! into this module are never reached; they are here so the layer has one
//! shape in both builds.

use std::ffi::c_void;
use std::sync::OnceLock;

use super::vk_layer::passes::Pass;

/// Whether the effects are built in: not here.
pub const AVAILABLE: bool = false;

/// The switches that would turn an effect on, and the mode that sets them.
const SWITCHES: [&str; 6] = [
    "INTOXICATED_SSR",
    "INTOXICATED_DOF",
    "INTOXICATED_BLOOM",
    "INTOXICATED_LENS",
    "INTOXICATED_SHARPEN",
    "INTOXICATED_SHADERS_MODE",
];

/// How a user adds them, for the log, devctl and the JSON.
const HOW_TO_ADD: &str = "run install.sh with --effects (intoxicated --update --effects) to add them";

/// What `--print-shader-looks` and `--print-ssr-looks` print.
pub const UNAVAILABLE_JSON: &str = "{\n  \"available\": false,\n  \"reason\": \"built without visual effects\",\n  \
     \"how_to_add\": \"run install.sh with --effects (intoxicated --update --effects)\"\n}";

/// The switches set to something that asks for an effect: empty, `0`, and a
/// mode of `off` ask for nothing.
fn asked_for(var: impl Fn(&str) -> Option<String>) -> Vec<&'static str> {
    SWITCHES
        .into_iter()
        .filter(|name| match var(name).as_deref().map(str::trim) {
            None | Some("" | "0") => false,
            Some(v) => !(*name == "INTOXICATED_SHADERS_MODE" && v.eq_ignore_ascii_case("off")),
        })
        .collect()
}

/// The log line for what [`asked_for`] found, or `None`.
fn ignored_line(names: &[&str]) -> Option<String> {
    (!names.is_empty())
        .then(|| format!("[android] vulkan: {} ignored: built without visual effects; {HOW_TO_ADD}", names.join(", ")))
}

/// Read the switches once and say so when one asks for an effect. Never on.
pub(crate) fn requested_at_start() -> bool {
    static SAID: OnceLock<()> = OnceLock::new();
    SAID.get_or_init(|| {
        if let Some(line) = ignored_line(&asked_for(|name| std::env::var(name).ok())) {
            println!("{line}");
        }
    });
    false
}

pub fn looks_json() -> String {
    UNAVAILABLE_JSON.into()
}

pub fn shader_looks_json() -> String {
    UNAVAILABLE_JSON.into()
}

/// devctl `ssr`, `dof`, `bloom`, `lens`, `sharpen`, `shaders` and `effects`:
/// not built.
pub fn control_verb(verb: &str, _args: &[&str]) -> Option<Result<String, String>> {
    matches!(verb, "ssr" | "dof" | "bloom" | "lens" | "sharpen" | "shaders" | "effects")
        .then(|| Err(format!("not built: this client was built without visual effects; {HOW_TO_ADD}")))
}

// The layer's calls, never reached (see the module comment).

/// The effects hotkey's toggle: never reached, as `effects_hotkey::load`
/// leaves the hotkey off when [`AVAILABLE`] is false.
pub(crate) fn toggle_all(_by: &str) {}

pub(crate) fn pass_begun(_pass: &Pass) {}
pub(crate) fn resolved(_cmd: *mut c_void, _src: u64, _dst: u64, _dst_layout: u32) {}
pub(crate) fn barriers(_cmd: *mut c_void, _list: impl Iterator<Item = (u64, u32)>) {}
pub(crate) fn written(_cmd: *mut c_void, _image: u64) {}
pub(crate) fn frame_end(_presents: u64) {}
pub(crate) fn image_destroyed(_image: u64) {}
pub(crate) fn device_destroying(_device: *mut c_void) {}
pub(crate) fn widen_usage(_device: *mut c_void, _info: *const c_void) -> Option<[u64; 11]> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let owned: Vec<(String, String)> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |name| owned.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
    }

    #[test]
    fn a_switch_that_asks_for_an_effect_is_named_once_in_one_line() {
        assert_eq!(asked_for(env(&[])), Vec::<&str>::new());
        assert_eq!(ignored_line(&asked_for(env(&[]))), None);
        let quiet = [("INTOXICATED_SSR", "0"), ("INTOXICATED_DOF", " "), ("INTOXICATED_SHADERS_MODE", "off")];
        assert_eq!(asked_for(env(&quiet)), Vec::<&str>::new());
        let loud = [("INTOXICATED_SSR", "1"), ("INTOXICATED_SHARPEN", "1"), ("INTOXICATED_SHADERS_MODE", "cinematic")];
        let names = asked_for(env(&loud));
        assert_eq!(names, ["INTOXICATED_SSR", "INTOXICATED_SHARPEN", "INTOXICATED_SHADERS_MODE"]);
        let line = ignored_line(&names).unwrap();
        assert!(line.contains("INTOXICATED_SSR, INTOXICATED_SHARPEN, INTOXICATED_SHADERS_MODE ignored"), "{line}");
        assert!(line.contains("built without visual effects"), "{line}");
        assert!(line.contains("--effects"), "{line}");
    }

    #[test]
    fn the_verbs_answer_not_built_and_others_are_not_theirs() {
        for verb in ["ssr", "dof", "bloom", "lens", "sharpen", "shaders", "effects"] {
            let answer = control_verb(verb, &["on"]).expect("an effect verb");
            assert!(answer.unwrap_err().starts_with("not built: "), "{verb}");
        }
        assert!(control_verb("passes", &[]).is_none());
    }

    #[test]
    fn the_tables_say_the_effects_are_unavailable() {
        for json in [shader_looks_json(), looks_json()] {
            let doc: serde_json::Value = serde_json::from_str(&json).expect("JSON");
            assert_eq!(doc["available"], false);
            assert!(doc.get("effects").is_none());
        }
        assert!(!requested_at_start());
    }
}
