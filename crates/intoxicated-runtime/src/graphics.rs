//! Which graphics backend the engine is offered.
//!
//! The engine links no Vulkan. It `dlopen`s `libvulkan.so`, then
//! `libvulkan.so.1`, and falls back to GLES3 when neither opens. This process
//! answers that `dlopen` (`android::vk_layer`), so withholding the host's
//! Vulkan loader is how GLES is chosen. The decision is made once per process,
//! from two inputs:
//!
//! 1. `INTOXICATED_GRAPHICS` (`automatic`, `vulkan` or `gles`), the Graphics
//!    setting. A backend named here wins outright; `automatic` is the absence
//!    of a choice, not a veto, so the flag below still counts; a word that is
//!    not a backend means Automatic and the flag is not consulted.
//! 2. The `IntoxicatedGraphicsBackend` key in the profile's `flags.json`,
//!    resolved through `flag_layers` like any client key. Only `flags.json`
//!    can carry it.
//!
//! With neither, the engine gets Automatic: Vulkan if the host has it, else
//! GLES3. There is no fallback from GLES back to Vulkan within a process: once
//! the loader is withheld there is nothing to fall back to.
//!
//! The engine's `Loaded <n> shaders from pack <name>` line says which backend
//! actually ran: `vulkan_mobile` or `glsles3`. `INTOXICATED_NO_VULKAN`
//! (`vk_layer.rs`) withholds Vulkan independently of this module.

use std::sync::OnceLock;

/// The environment switch.
pub const ENV: &str = "INTOXICATED_GRAPHICS";
/// The flag key. The `Intoxicated` prefix is what makes `flag_layers` treat
/// it as a client key that never reaches the engine.
pub const FLAG_KEY: &str = "IntoxicatedGraphicsBackend";

/// The backend words, for messages.
const KNOWN: &str = "automatic, vulkan, gles";

/// What the engine is offered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    /// Vulkan if the host loader opens, else GLES3: the engine's own choice.
    Automatic,
    /// Vulkan, as asked.
    Vulkan,
    /// GLES3, with the Vulkan loader withheld.
    Gles,
}

impl Backend {
    /// Parse a backend word, ignoring case and surrounding whitespace. Empty
    /// is Automatic; a word that names no backend is `None`.
    pub fn parse(word: &str) -> Option<Backend> {
        match word.trim().to_ascii_lowercase().as_str() {
            "" | "automatic" | "auto" => Some(Backend::Automatic),
            "vulkan" => Some(Backend::Vulkan),
            "gles" | "gles3" | "glsles3" | "opengl" | "opengles" => Some(Backend::Gles),
            _ => None,
        }
    }

    /// Whether the engine may open the host's Vulkan loader.
    pub fn offers_vulkan(self) -> bool {
        !matches!(self, Backend::Gles)
    }

    /// How the startup line names it.
    pub fn label(self) -> &'static str {
        match self {
            Backend::Automatic => "automatic (Vulkan if available, else GLES3)",
            Backend::Vulkan => "Vulkan",
            Backend::Gles => "GLES3; Vulkan is being withheld deliberately",
        }
    }
}

/// The decision and what made it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
    pub backend: Backend,
    /// `the Graphics setting`, the flag layer's description, `Automatic`, or
    /// `Automatic (after a bad INTOXICATED_GRAPHICS)`.
    pub source: String,
}

/// Decide from the variable's text and, only when the variable leaves the
/// choice open, the flag key's `(source, value)`. Returns the choice and the
/// lines to print on stderr.
pub fn decide(env: Option<&str>, flag: impl FnOnce() -> Option<(String, String)>) -> (Choice, Vec<String>) {
    let mut warnings = Vec::new();
    if let Some(text) = env.map(str::trim).filter(|t| !t.is_empty()) {
        match Backend::parse(text) {
            Some(backend @ (Backend::Vulkan | Backend::Gles)) => {
                return (Choice { backend, source: "the Graphics setting".into() }, warnings);
            }
            Some(Backend::Automatic) => {}
            None => {
                warnings.push(format!("[graphics] {ENV}={text:?} is not a backend; using Automatic. Known: {KNOWN}"));
                let source = format!("Automatic (after a bad {ENV})");
                return (Choice { backend: Backend::Automatic, source }, warnings);
            }
        }
    }
    if let Some((source, value)) = flag() {
        match Backend::parse(&value) {
            Some(backend @ (Backend::Vulkan | Backend::Gles)) => return (Choice { backend, source }, warnings),
            Some(Backend::Automatic) => {}
            None => warnings.push(format!(
                "[graphics] {source} asked for {FLAG_KEY}={value:?}, which is not a backend; ignoring"
            )),
        }
    }
    (Choice { backend: Backend::Automatic, source: "Automatic".into() }, warnings)
}

/// The flag key's source and value, read from the profile now.
fn flag_request() -> Option<(String, String)> {
    let resolved = crate::flag_layers::resolve(crate::flag_layers::collect());
    resolved.get(FLAG_KEY).map(|r| (r.source.describe(), r.value.clone()))
}

/// The decision for this process, made on the first call. Its warnings go to
/// stderr then, once.
pub fn choice() -> &'static Choice {
    static CHOICE: OnceLock<Choice> = OnceLock::new();
    CHOICE.get_or_init(|| {
        let env = std::env::var_os(ENV).map(|v| v.to_string_lossy().into_owned());
        let (choice, warnings) = decide(env.as_deref(), flag_request);
        for w in warnings {
            eprintln!("{w}");
        }
        choice
    })
}

/// The one startup line saying what the engine is offered and why.
pub fn report() {
    let c = choice();
    println!("[graphics] backend: {}, from {}", c.backend.label(), c.source);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(value: &str) -> impl FnOnce() -> Option<(String, String)> {
        let value = value.to_string();
        move || Some(("user".to_string(), value))
    }

    fn decided(env: Option<&str>, flag: impl FnOnce() -> Option<(String, String)>) -> (Backend, String) {
        let (c, _) = decide(env, flag);
        (c.backend, c.source)
    }

    #[test]
    fn words_parse_to_backends() {
        for gles in ["gles", "GLES", " GlEs3 ", "opengl", "glsles3", "opengles"] {
            assert_eq!(Backend::parse(gles), Some(Backend::Gles), "{gles:?}");
        }
        assert_eq!(Backend::parse("Vulkan"), Some(Backend::Vulkan));
        assert_eq!(Backend::parse(""), Some(Backend::Automatic));
        assert_eq!(Backend::parse("auto"), Some(Backend::Automatic));
        assert_eq!(Backend::parse("metal"), None);
        assert!(Backend::Automatic.offers_vulkan());
        assert!(Backend::Vulkan.offers_vulkan());
        assert!(!Backend::Gles.offers_vulkan());
    }

    #[test]
    fn the_variable_wins_and_automatic_defers_to_the_flag() {
        assert_eq!(decided(None, || None), (Backend::Automatic, "Automatic".into()));
        assert_eq!(decided(Some("vulkan"), user("gles")), (Backend::Vulkan, "the Graphics setting".into()));
        assert_eq!(decided(Some("automatic"), user("gles")), (Backend::Gles, "user".into()));
        assert_eq!(decided(None, user("gles")), (Backend::Gles, "user".into()));
        assert_eq!(decided(Some("  "), user("gles")), (Backend::Gles, "user".into()));
    }

    #[test]
    fn a_bad_variable_means_automatic_and_names_the_real_variable() {
        let bad = (Backend::Automatic, "Automatic (after a bad INTOXICATED_GRAPHICS)".to_string());
        assert_eq!(decided(Some("mantle"), || None), bad);
        let consulted = std::cell::Cell::new(false);
        let (c, warnings) = decide(Some(" mantle "), || {
            consulted.set(true);
            Some(("user".into(), "gles".into()))
        });
        assert_eq!((c.backend, c.source), bad);
        assert!(!consulted.get(), "a bad variable is not overruled by the flag");
        assert_eq!(
            warnings,
            ["[graphics] INTOXICATED_GRAPHICS=\"mantle\" is not a backend; using Automatic. Known: automatic, vulkan, gles"]
        );
    }

    #[test]
    fn a_bad_flag_is_ignored_with_a_line_and_automatic_silently() {
        let (c, warnings) = decide(None, user("mantle"));
        assert_eq!((c.backend, c.source.as_str()), (Backend::Automatic, "Automatic"));
        assert_eq!(
            warnings,
            ["[graphics] user asked for IntoxicatedGraphicsBackend=\"mantle\", which is not a backend; ignoring"]
        );
        let (c, warnings) = decide(None, user("automatic"));
        assert_eq!(c.backend, Backend::Automatic);
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn the_lines_have_no_dashes_and_the_key_is_a_client_key() {
        for b in [Backend::Automatic, Backend::Vulkan, Backend::Gles] {
            assert!(!b.label().contains(['\u{2013}', '\u{2014}']), "{}", b.label());
        }
        assert_eq!(Backend::Gles.label(), "GLES3; Vulkan is being withheld deliberately");
        assert!(crate::flag_layers::is_client_key(FLAG_KEY));
    }
}
