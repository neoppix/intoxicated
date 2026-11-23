//! The user's `flags.json`: where it is, how it reads, how it is written.
//!
//! The file is a flat JSON object of flag name to value, as Bloxstrap exports
//! it. Roblox keeps every setting as a string, so `true` and `12` are taken as
//! `"true"` and `"12"`. Nested values and `null` have no string form and are
//! refused by name.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The variable that names the flags file outright.
pub const PATH_VARIABLE: &str = "INTOXICATED_FLAGS";

/// `<profile>/flags.json`, unless `INTOXICATED_FLAGS` names a file outright (a
/// development switch: one file for every profile).
pub fn path_in(profile_dir: &Path) -> PathBuf {
    match std::env::var_os(PATH_VARIABLE) {
        Some(p) => PathBuf::from(p),
        None => profile_dir.join("flags.json"),
    }
}

/// Parse a flags document. Blank text is an empty set, not an error.
pub fn parse(text: &str) -> Result<BTreeMap<String, String>, String> {
    if text.trim().is_empty() {
        return Ok(BTreeMap::new());
    }
    let doc: serde_json::Value = serde_json::from_str(text).map_err(|e| format!("not valid JSON: {e}"))?;
    let serde_json::Value::Object(map) = doc else {
        return Err("the document must be a JSON object of flag names to values".into());
    };
    let mut out = BTreeMap::new();
    for (name, value) in map {
        if name.trim().is_empty() {
            return Err("a flag with an empty name".into());
        }
        let value = match value {
            serde_json::Value::String(s) => s,
            v @ (serde_json::Value::Bool(_) | serde_json::Value::Number(_)) => v.to_string(),
            _ => return Err(format!("{name}: a flag value must be text, a number or true/false")),
        };
        out.insert(name, value);
    }
    Ok(out)
}

/// Replace the document at `path`. Written beside it and renamed in, so a
/// reader never sees half a file.
pub fn write(path: &Path, values: &BTreeMap<String, String>) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let body = serde_json::to_string_pretty(values).map_err(|e| e.to_string())? + "\n";
    let tmp = path.with_extension("json.new");
    std::fs::write(&tmp, body).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_document_round_trips_with_values_as_strings() {
        let dir = std::env::temp_dir().join(format!("intox-flagfile-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("flags.json");

        let values = parse(r#"{"FFlagA": true, "DFIntB": 12, "FStringC": "x"}"#).unwrap();
        assert_eq!(values["FFlagA"], "true");
        assert_eq!(values["DFIntB"], "12");
        assert_eq!(values["FStringC"], "x");

        write(&path, &values).unwrap();
        let back = parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(back, values);
        assert!(!path.with_extension("json.new").exists(), "the temporary must be renamed away");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn what_has_no_string_form_is_refused() {
        assert!(parse("[1, 2]").is_err());
        assert!(parse(r#"{"A": null}"#).unwrap_err().starts_with("A:"));
        assert!(parse(r#"{"A": {"b": 1}}"#).is_err());
        assert!(parse("   ").unwrap().is_empty());
    }
}
