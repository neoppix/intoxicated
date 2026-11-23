//! The pinned Roblox signing certificates (SHA-256 of the DER).
//!
//! The list lives in `packaging/trust/roblox-signing-certificates.json` and is
//! compiled into the binary. `INTOXICATED_TRUST_FILE` may name another JSON
//! file of the same shape for auditing; if that file cannot be read or is not
//! JSON, nothing is trusted.

const EMBEDDED: &str = include_str!("../../../packaging/trust/roblox-signing-certificates.json");
pub const OVERRIDE_ENV: &str = "INTOXICATED_TRUST_FILE";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pins(Vec<String>);

impl Pins {
    /// Build from hex strings; anything that is not 64 hex digits after
    /// trimming is dropped.
    pub fn new<I: IntoIterator<Item = S>, S: AsRef<str>>(items: I) -> Pins {
        Pins(
            items
                .into_iter()
                .map(|s| s.as_ref().trim().to_ascii_lowercase())
                .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
                .collect(),
        )
    }

    pub fn from_json(text: &str) -> Pins {
        let Ok(doc) = serde_json::from_str::<serde_json::Value>(text) else {
            return Pins::default();
        };
        let list = doc.get("certificates").and_then(|c| c.as_array()).cloned().unwrap_or_default();
        Pins::new(list.iter().filter_map(|c| c.get("sha256")?.as_str().map(str::to_owned)))
    }

    pub fn embedded() -> Pins {
        Pins::from_json(EMBEDDED)
    }

    /// The embedded list, unless the override variable names a file.
    pub fn load() -> Pins {
        match std::env::var_os(OVERRIDE_ENV) {
            Some(path) => std::fs::read_to_string(path).map(|t| Pins::from_json(&t)).unwrap_or_default(),
            None => Pins::embedded(),
        }
    }

    pub fn contains(&self, sha256_hex: &str) -> bool {
        self.0.iter().any(|p| p.eq_ignore_ascii_case(sha256_hex))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub const ORIGINAL: &str = "44932ea35a17a267372d71b54d1a0cb3da0dca5113e94406ae2fe18090ba1477";
    pub const ROTATED: &str = "2bebd189e8d3106401347056c93d045b61e20e22d0c3cbed85474aeb00a3d12a";

    #[test]
    fn embedded_has_both_roblox_certificates() {
        let p = Pins::embedded();
        assert!(p.contains(ORIGINAL));
        assert!(p.contains(ROTATED));
        assert!(p.contains(&ORIGINAL.to_ascii_uppercase()));
    }

    #[test]
    fn malformed_entries_are_dropped() {
        let p = Pins::from_json(r#"{"certificates":[{"sha256":"abc"},{"sha256":"  ZZ"},{"nope":1}]}"#);
        assert!(p.is_empty());
        assert!(Pins::from_json("not json").is_empty());
        let p = Pins::from_json(&format!(r#"{{"certificates":[{{"sha256":" {} "}}]}}"#, ORIGINAL.to_uppercase()));
        assert!(p.contains(ORIGINAL));
    }
}
