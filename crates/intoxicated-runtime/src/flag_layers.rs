//! Flag overrides: where they come from and which one wins.
//!
//! Two layers, lowest precedence first:
//!
//! 1. [`Source::BuiltIn`]: [`BUILT_IN`], one entry.
//! 2. [`Source::User`]: the profile's `flags.json` (see [`crate::flag_file`]).
//!
//! A later layer replaces an earlier one's value for the same key, so the user
//! always wins, and the values it replaced are kept in reading order so a
//! conflict can be reported. Nothing is cached: [`collect`] rereads
//! `flags.json` on every call, because a launch asks several times (graphics
//! backend, present mode, each settings load) and each answer has to reflect
//! the file as it is now.
//!
//! Keys starting with [`CLIENT_PREFIX`] belong to this client, not to Roblox.
//! They resolve like any other key so the rest of the runtime can read them
//! with their provenance, and [`crate::roblox_settings`] never sends them to
//! the engine.
//!
//! Values are never normalised: `"7"` and `"Verbose"` pass through as written.
//!
//! **Reading `flags.json`.** Absent is no user layer, silently. Text that is
//! not JSON, or JSON that is not an object, is ignored with a line. An object
//! goes through [`crate::flag_file::parse`], the same reader the file's own
//! writer is paired with: strings as they are, numbers and booleans as their
//! JSON text, and a document holding a `null`, an array or an object as a
//! value is refused whole, with a line naming the key, rather than sending
//! that value's JSON text to the engine. Blank text is an empty layer.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The prefix of the keys that are this client's own settings.
pub const CLIENT_PREFIX: &str = "Intoxicated";

/// The built-in layer.
///
/// `FFlagUserLaunchedWithBloxstrap` is not an engine flag; it is the marker
/// desktop launchers set. INFERRED that experiences can read it. A user turns
/// it off with `"FFlagUserLaunchedWithBloxstrap": "False"` in `flags.json`.
pub const BUILT_IN: [(&str, &str); 1] = [("FFlagUserLaunchedWithBloxstrap", "True")];

/// Where a value came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    BuiltIn,
    User,
}

impl Source {
    /// The word reports use for this source.
    pub fn describe(&self) -> String {
        match self {
            Source::BuiltIn => "built-in",
            Source::User => "user",
        }
        .to_string()
    }
}

/// One layer's values, in the order they were read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layer {
    pub source: Source,
    pub values: Vec<(String, String)>,
}

/// The value that won for one key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub value: String,
    pub source: Source,
    /// The values this one replaced, oldest first.
    pub overridden: Vec<(Source, String)>,
}

/// Whether `key` is one of this client's own settings rather than a Roblox
/// flag.
pub fn is_client_key(key: &str) -> bool {
    key.starts_with(CLIENT_PREFIX)
}

/// The file the user layer is read from.
pub fn user_file() -> PathBuf {
    crate::flag_file::path_in(&crate::profile::active())
}

/// Every layer, lowest precedence first, read now.
pub fn collect() -> Vec<Layer> {
    collect_from(&user_file())
}

/// [`collect`] against an explicit user file.
pub fn collect_from(user_file: &Path) -> Vec<Layer> {
    let mut layers = vec![Layer {
        source: Source::BuiltIn,
        values: BUILT_IN.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
    }];
    if let Some(values) = read_user_file(user_file) {
        layers.push(Layer { source: Source::User, values });
    }
    layers
}

/// Fold the layers into one value per key.
pub fn resolve(layers: Vec<Layer>) -> BTreeMap<String, Resolved> {
    let mut out: BTreeMap<String, Resolved> = BTreeMap::new();
    for layer in layers {
        for (key, value) in layer.values {
            match out.get_mut(&key) {
                Some(entry) => {
                    let old_source = std::mem::replace(&mut entry.source, layer.source.clone());
                    let old_value = std::mem::replace(&mut entry.value, value);
                    entry.overridden.push((old_source, old_value));
                }
                None => {
                    out.insert(key, Resolved { value, source: layer.source.clone(), overridden: Vec::new() });
                }
            }
        }
    }
    out
}

/// The user layer's values, or `None` when there is no usable file.
fn read_user_file(path: &Path) -> Option<Vec<(String, String)>> {
    let text = std::fs::read_to_string(path).ok()?;
    match parse_user_text(&text) {
        Ok(values) => Some(values),
        Err(why) => {
            println!("  flags: {} {why}; ignoring", path.display());
            None
        }
    }
}

/// Parse `flags.json`'s text. The error completes the sentence
/// "flags: <path> ...".
fn parse_user_text(text: &str) -> Result<Vec<(String, String)>, String> {
    if !text.trim().is_empty() {
        match serde_json::from_str::<serde_json::Value>(text) {
            Err(e) => return Err(format!("is not valid JSON ({e})")),
            Ok(v) if !v.is_object() => return Err("is not a JSON object".into()),
            Ok(_) => {}
        }
    }
    crate::flag_file::parse(text)
        .map(|map| map.into_iter().collect())
        .map_err(|e| format!("was refused ({e})"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer(source: Source, values: &[(&str, &str)]) -> Layer {
        Layer { source, values: values.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect() }
    }

    #[test]
    fn the_user_wins_and_what_it_replaced_is_kept() {
        let resolved = resolve(vec![
            layer(Source::BuiltIn, &[("FFlagUserLaunchedWithBloxstrap", "True"), ("A", "1")]),
            layer(Source::User, &[("FFlagUserLaunchedWithBloxstrap", "False"), ("B", "2")]),
        ]);
        let bloxstrap = &resolved["FFlagUserLaunchedWithBloxstrap"];
        assert_eq!(bloxstrap.value, "False");
        assert_eq!(bloxstrap.source, Source::User);
        assert_eq!(bloxstrap.overridden, vec![(Source::BuiltIn, "True".to_string())]);
        assert_eq!(resolved["A"].source, Source::BuiltIn);
        assert!(resolved["A"].overridden.is_empty());
        assert_eq!(resolved["B"].value, "2");
        assert_eq!(resolved.len(), 3);
    }

    #[test]
    fn three_layers_keep_the_losers_in_reading_order() {
        let resolved = resolve(vec![
            layer(Source::BuiltIn, &[("K", "first")]),
            layer(Source::User, &[("K", "second")]),
            layer(Source::User, &[("K", "third")]),
        ]);
        assert_eq!(resolved["K"].value, "third");
        assert_eq!(
            resolved["K"].overridden,
            vec![(Source::BuiltIn, "first".to_string()), (Source::User, "second".to_string())]
        );
    }

    #[test]
    fn the_built_in_layer_is_always_first_and_the_file_is_reread() {
        let dir = std::env::temp_dir().join(format!("intox-flaglayers-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("flags.json");

        // Absent: the built-in layer alone.
        let layers = collect_from(&file);
        assert_eq!(layers.len(), 1);
        assert_eq!(layers[0].source, Source::BuiltIn);
        assert_eq!(layers[0].values, vec![("FFlagUserLaunchedWithBloxstrap".into(), "True".into())]);

        // The launcher's layout, exactly as it is on disk today.
        std::fs::write(
            &file,
            "{\"DFIntTaskSchedulerTargetFps\":\"9999\",\"FIntRenderForceVideoMemorySize\":\"1610612736\"}\n",
        )
        .unwrap();
        let resolved = resolve(collect_from(&file));
        assert_eq!(resolved["DFIntTaskSchedulerTargetFps"].value, "9999");
        assert_eq!(resolved["DFIntTaskSchedulerTargetFps"].source, Source::User);
        assert_eq!(resolved.len(), 3);

        // Edited between two reads: the second read sees the edit.
        std::fs::write(&file, "{\n  \"DFIntTaskSchedulerTargetFps\": 144,\n  \"FFlagX\": true\n}").unwrap();
        let resolved = resolve(collect_from(&file));
        assert_eq!(resolved["DFIntTaskSchedulerTargetFps"].value, "144");
        assert_eq!(resolved["FFlagX"].value, "true");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_bad_file_drops_the_user_layer_only() {
        assert_eq!(parse_user_text("{not json").unwrap_err().split(" (").next(), Some("is not valid JSON"));
        assert_eq!(parse_user_text("[1,2]").unwrap_err(), "is not a JSON object");
        assert!(parse_user_text(r#"{"A": null}"#).unwrap_err().starts_with("was refused (A:"));
        assert!(parse_user_text("").unwrap().is_empty());
        assert_eq!(parse_user_text(r#"{"FStringX":"Verbose","DFIntY":"7"}"#).unwrap().len(), 2);

        let dir = std::env::temp_dir().join(format!("intox-flaglayers-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("flags.json");
        std::fs::write(&file, "{\"DFIntTaskSchedulerTargetFps\":").unwrap();
        let layers = collect_from(&file);
        assert_eq!(layers.len(), 1, "a malformed file must leave the built-ins in force");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn client_keys_are_recognised_by_prefix() {
        assert!(is_client_key("IntoxicatedGraphicsBackend"));
        assert!(is_client_key("IntoxicatedPresentMode"));
        assert!(!is_client_key("DFIntTaskSchedulerTargetFps"));
        assert!(!is_client_key("intoxicatedLowercase"));
    }
}
