//! Which present mode a swapchain gets.
//!
//! **The default keeps the engine's IMMEDIATE.** This Roblox build asks for
//! IMMEDIATE with three images, and on this machine (NVIDIA 580, Xorg, i3,
//! picom) that is about 240 presents a second. Every vblank-paced mode runs at
//! about one present a second here, for any Vulkan client: FIFO and
//! FIFO_RELAXED gave 1.0 presents/s in the client, windowed and fullscreen,
//! and vkcube did 8 frames in 7.2 s with FIFO against 2000 in 0.62 s with
//! IMMEDIATE (measured). So the default never picks FIFO or FIFO_RELAXED, and
//! only ever replaces an engine choice that is vblank-paced itself: an
//! engine asking for FIFO gets MAILBOX where the driver offers it.
//!
//! `INTOXICATED_PRESENT_MODE` overrides that, and a plugin can ask through the
//! `IntoxicatedPresentMode` flag key (a client key the flag layers keep away
//! from Roblox); the variable wins over the key.

use super::{VK_PRESENT_MODE_FIFO, VK_PRESENT_MODE_FIFO_RELAXED, VK_PRESENT_MODE_IMMEDIATE, VK_PRESENT_MODE_MAILBOX};

/// The environment switch.
pub(super) const ENV: &str = "INTOXICATED_PRESENT_MODE";
/// The flag layers' key for the same setting.
pub(super) const FLAG_KEY: &str = "IntoxicatedPresentMode";

const KNOWN: &str = "off, auto, mailbox, immediate, uncapped, fifo, fifo-relaxed";

/// A present mode as log lines spell it.
pub(super) fn mode_name(mode: i32) -> &'static str {
    match mode {
        VK_PRESENT_MODE_IMMEDIATE => "IMMEDIATE",
        VK_PRESENT_MODE_MAILBOX => "MAILBOX",
        VK_PRESENT_MODE_FIFO => "FIFO",
        VK_PRESENT_MODE_FIFO_RELAXED => "FIFO_RELAXED",
        1_000_111_000 => "SHARED_DEMAND_REFRESH",
        1_000_111_001 => "SHARED_CONTINUOUS_REFRESH",
        1_000_361_000 => "FIFO_LATEST_READY",
        _ => "unknown",
    }
}

/// Whether a mode waits for vblank.
fn vblank_paced(mode: i32) -> bool {
    matches!(mode, VK_PRESENT_MODE_FIFO | VK_PRESENT_MODE_FIFO_RELAXED)
}

/// What to do with the engine's present mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Policy {
    /// `off` / `engine`: forward the engine's choice untouched.
    Engine,
    /// The default: keep the engine's choice unless it is vblank-paced, and
    /// then take MAILBOX if the driver offers it.
    Auto,
    /// Asked for by name: the first of these the driver offers, else the
    /// engine's choice.
    Prefer(Vec<i32>),
}

impl Policy {
    /// The modes this policy would substitute, best first.
    pub(super) fn preferences(&self) -> &[i32] {
        match self {
            Policy::Engine => &[],
            Policy::Auto => &[VK_PRESENT_MODE_MAILBOX],
            Policy::Prefer(v) => v,
        }
    }
}

/// One spelling, trimmed and in any case, or `None` when it is not one.
pub(super) fn parse(v: &str) -> Option<Policy> {
    Some(match v.trim().to_ascii_lowercase().as_str() {
        "" | "auto" => Policy::Auto,
        "off" | "engine" => Policy::Engine,
        "mailbox" => Policy::Prefer(vec![VK_PRESENT_MODE_MAILBOX]),
        "immediate" => Policy::Prefer(vec![VK_PRESENT_MODE_IMMEDIATE]),
        "uncapped" => Policy::Prefer(vec![VK_PRESENT_MODE_MAILBOX, VK_PRESENT_MODE_IMMEDIATE]),
        "fifo" => Policy::Prefer(vec![VK_PRESENT_MODE_FIFO]),
        "fifo-relaxed" | "fifo_relaxed" => Policy::Prefer(vec![VK_PRESENT_MODE_FIFO_RELAXED]),
        _ => return None,
    })
}

/// The policy in force, where it came from (for the once-per-process line)
/// and anything worth warning about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Resolved {
    pub(super) policy: Policy,
    pub(super) source: String,
    pub(super) warnings: Vec<String>,
}

/// Decide from the environment value and the flag layers' `(source, value)`.
///
/// An empty variable is unset, and `auto` in it defers to the flag layers;
/// any other unreadable value is reported and means auto without consulting
/// them, so a typo is never silently overruled by a plugin.
pub(super) fn resolve(env: Option<&str>, flag: Option<(&str, &str)>) -> Resolved {
    let mut warnings = Vec::new();
    let (policy, source) = 'decided: {
        if let Some(v) = env.map(str::trim).filter(|v| !v.is_empty()) {
            if !v.eq_ignore_ascii_case("auto") {
                match parse(v) {
                    Some(p) => break 'decided (p, format!("{ENV}={v}")),
                    None => {
                        warnings.push(format!(
                            "[android] vulkan: {ENV}={v:?} is not a present mode; using auto. Known: {KNOWN}"
                        ));
                        break 'decided (Policy::Auto, format!("auto, after an unusable {ENV}"));
                    }
                }
            }
        }
        if let Some((from, v)) = flag {
            match parse(v) {
                Some(p) => break 'decided (p, format!("{FLAG_KEY}={} from {from}", v.trim())),
                None => {
                    warnings.push(format!(
                        "[android] vulkan: {FLAG_KEY}={v:?} from {from} is not a present mode; using auto. Known: {KNOWN}"
                    ));
                    break 'decided (Policy::Auto, format!("auto, after an unusable {FLAG_KEY}"));
                }
            }
        }
        (Policy::Auto, "auto".to_string())
    };
    if policy.preferences().iter().copied().any(vblank_paced) {
        warnings.push(
            "[android] vulkan: FIFO and FIFO_RELAXED wait for vblank, which on some Xorg setups (NVIDIA here, \
             vkcube included) means about 1 present per second; unset the setting if the game crawls"
                .into(),
        );
    }
    Resolved { policy, source, warnings }
}

/// The mode a swapchain gets, from what the engine asked for and what the
/// driver lists.
pub(super) fn choose(policy: &Policy, engine: i32, driver: &[i32]) -> i32 {
    match policy {
        Policy::Engine => engine,
        Policy::Auto if !vblank_paced(engine) => engine,
        _ => policy.preferences().iter().copied().find(|m| driver.contains(m)).unwrap_or(engine),
    }
}

/// What one swapchain is created with, and the lines that say so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Decision {
    pub(super) mode: i32,
    pub(super) min_image_count: u32,
    pub(super) lines: Vec<String>,
}

/// The whole per-swapchain decision. `driver` is the driver's present-mode
/// list, `None` when it could not be asked; `max_images` asks the surface
/// for `maxImageCount` (0 is no limit), and is only called when a
/// replacement needs a spare image.
///
/// The lines are printed whatever the trace switches say, because "what did
/// the client substitute" is exactly what a report about tearing or stutter
/// needs, and a trace flag would hide it.
pub(super) fn decide(
    policy: &Policy,
    engine_mode: i32,
    engine_count: u32,
    driver: Option<&[i32]>,
    max_images: impl FnOnce() -> Option<u32>,
) -> Decision {
    let asked = mode_name(engine_mode);
    let keep = |line: String| Decision { mode: engine_mode, min_image_count: engine_count, lines: vec![line] };
    if *policy == Policy::Engine {
        return keep(format!(
            "[android] vulkan: swapchain present mode {asked} (the engine's own choice; {ENV}=off)"
        ));
    }
    let Some(driver) = driver else {
        return keep(format!(
            "[android] vulkan: swapchain present mode {asked} (could not ask the driver what it supports, so the \
             engine's own choice stands)"
        ));
    };
    let offered = driver.iter().map(|&m| mode_name(m)).collect::<Vec<_>>().join(", ");
    let chosen = choose(policy, engine_mode, driver);
    if chosen == engine_mode {
        return keep(format!("[android] vulkan: swapchain present mode {asked} (unchanged; driver offers {offered})"));
    }
    let mut lines =
        vec![format!("[android] vulkan: swapchain present mode {asked} -> {} (driver offers {offered})", mode_name(chosen))];
    let mut count = engine_count;
    if !vblank_paced(chosen) && engine_count < 3 {
        match max_images() {
            Some(max) if max == 0 || max >= 3 => {
                count = 3;
                lines.push(format!(
                    "[android] vulkan: minImageCount {engine_count} -> 3 (a replacement mode needs a spare image; \
                     the surface allows {})",
                    if max == 0 { "any number".to_string() } else { format!("up to {max}") }
                ));
            }
            Some(max) => lines.push(format!(
                "[android] vulkan: minImageCount {engine_count} kept (the surface allows at most {max}, so {} runs \
                 without a spare image)",
                mode_name(chosen)
            )),
            None => lines.push(format!(
                "[android] vulkan: minImageCount {engine_count} kept (could not ask the surface how many images it \
                 allows)"
            )),
        }
    }
    Decision { mode: chosen, min_image_count: count, lines }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IMMEDIATE: i32 = VK_PRESENT_MODE_IMMEDIATE;
    const MAILBOX: i32 = VK_PRESENT_MODE_MAILBOX;
    const FIFO: i32 = VK_PRESENT_MODE_FIFO;
    const FIFO_RELAXED: i32 = VK_PRESENT_MODE_FIFO_RELAXED;
    /// What this machine's NVIDIA driver lists for the window's surface.
    const NVIDIA: [i32; 4] = [FIFO, FIFO_RELAXED, IMMEDIATE, 1_000_361_000];

    fn policy(env: Option<&str>, flag: Option<(&str, &str)>) -> Policy {
        resolve(env, flag).policy
    }

    #[test]
    fn spellings() {
        assert_eq!(policy(None, None), Policy::Auto);
        assert_eq!(policy(Some(""), None), Policy::Auto, "empty is unset");
        assert_eq!(policy(Some("  "), None), Policy::Auto);
        assert_eq!(Policy::Auto.preferences(), &[MAILBOX]);
        assert_eq!(policy(Some("fifo"), None), Policy::Prefer(vec![FIFO]));
        assert_eq!(policy(Some("MailBox"), None), Policy::Prefer(vec![MAILBOX]));
        assert_eq!(policy(Some(" immediate "), None), Policy::Prefer(vec![IMMEDIATE]));
        assert_eq!(policy(Some("uncapped"), None), Policy::Prefer(vec![MAILBOX, IMMEDIATE]));
        assert_eq!(policy(Some("fifo-relaxed"), None), Policy::Prefer(vec![FIFO_RELAXED]));
        assert_eq!(policy(Some("fifo_relaxed"), None), Policy::Prefer(vec![FIFO_RELAXED]));
        assert_eq!(policy(Some("off"), None), Policy::Engine);
        assert_eq!(policy(Some("engine"), None), Policy::Engine);
    }

    #[test]
    fn the_variable_beats_the_flag_and_auto_lets_the_flag_through() {
        let plugin = Some(("plugin:fast", "immediate"));
        let r = resolve(None, plugin);
        assert_eq!(r.policy, Policy::Prefer(vec![IMMEDIATE]));
        assert_eq!(r.source, "IntoxicatedPresentMode=immediate from plugin:fast");
        let r = resolve(Some("fifo"), plugin);
        assert_eq!(r.policy, Policy::Prefer(vec![FIFO]));
        assert_eq!(r.source, "INTOXICATED_PRESENT_MODE=fifo");
        assert_eq!(resolve(Some("auto"), plugin).policy, Policy::Prefer(vec![IMMEDIATE]));
        assert_eq!(resolve(None, None).source, "auto");
    }

    #[test]
    fn an_unreadable_value_is_reported_and_means_auto() {
        let r = resolve(Some("imediate"), Some(("plugin:fast", "immediate")));
        assert_eq!(r.policy, Policy::Auto, "a typo is not overruled by the plugin");
        assert_eq!(r.source, "auto, after an unusable INTOXICATED_PRESENT_MODE");
        assert_eq!(
            r.warnings,
            vec!["[android] vulkan: INTOXICATED_PRESENT_MODE=\"imediate\" is not a present mode; using auto. \
                  Known: off, auto, mailbox, immediate, uncapped, fifo, fifo-relaxed"
                .to_string()]
        );
        let r = resolve(None, Some(("plugin:fast", "as-fast-as-possible")));
        assert_eq!(r.policy, Policy::Auto);
        assert_eq!(r.source, "auto, after an unusable IntoxicatedPresentMode");
        assert!(r.warnings[0].contains("IntoxicatedPresentMode=\"as-fast-as-possible\" from plugin:fast"));
    }

    #[test]
    fn asking_for_a_vblank_paced_mode_carries_a_warning_and_the_default_does_not() {
        assert!(resolve(None, None).warnings.is_empty());
        assert!(resolve(Some("immediate"), None).warnings.is_empty());
        assert_eq!(resolve(Some("fifo"), None).warnings.len(), 1);
        assert_eq!(resolve(None, Some(("plugin:x", "fifo-relaxed"))).warnings.len(), 1);
    }

    #[test]
    fn the_flag_key_is_a_client_key() {
        // `flag_layers::is_client_key` filters on the `Intoxicated` prefix,
        // which is what keeps this key away from the engine.
        assert!(FLAG_KEY.starts_with(crate::flag_layers::CLIENT_PREFIX));
        assert!(crate::flag_layers::is_client_key(FLAG_KEY));
    }

    #[test]
    fn the_default_keeps_the_engines_immediate_on_this_driver() {
        assert_eq!(choose(&Policy::Auto, IMMEDIATE, &NVIDIA), IMMEDIATE);
        // Even where MAILBOX is on offer: IMMEDIATE is not vblank-paced.
        assert_eq!(choose(&Policy::Auto, IMMEDIATE, &[IMMEDIATE, MAILBOX, FIFO]), IMMEDIATE);
        // It never picks FIFO or FIFO_RELAXED by itself.
        for engine in [IMMEDIATE, MAILBOX] {
            let c = choose(&Policy::Auto, engine, &NVIDIA);
            assert!(c != FIFO && c != FIFO_RELAXED);
        }
        // An engine asking for FIFO gets MAILBOX where there is one, and
        // keeps FIFO where there is none (this driver).
        assert_eq!(choose(&Policy::Auto, FIFO, &[IMMEDIATE, MAILBOX, FIFO]), MAILBOX);
        assert_eq!(choose(&Policy::Auto, FIFO, &NVIDIA), FIFO);
    }

    #[test]
    fn the_choice_table() {
        let prefer = |v: &[i32]| Policy::Prefer(v.to_vec());
        assert_eq!(choose(&prefer(&[MAILBOX]), IMMEDIATE, &NVIDIA), IMMEDIATE);
        assert_eq!(choose(&prefer(&[FIFO]), IMMEDIATE, &NVIDIA), FIFO);
        assert_eq!(choose(&prefer(&[MAILBOX, IMMEDIATE]), FIFO, &NVIDIA), IMMEDIATE);
        assert_eq!(choose(&Policy::Engine, IMMEDIATE, &NVIDIA), IMMEDIATE);
    }

    #[test]
    fn the_lines_and_the_image_count() {
        let d = decide(&Policy::Auto, IMMEDIATE, 3, Some(&NVIDIA), || panic!("not asked"));
        assert_eq!((d.mode, d.min_image_count), (IMMEDIATE, 3));
        assert_eq!(
            d.lines,
            vec!["[android] vulkan: swapchain present mode IMMEDIATE (unchanged; driver offers FIFO, FIFO_RELAXED, \
                  IMMEDIATE, FIFO_LATEST_READY)"
                .to_string()]
        );

        let llvmpipe = [IMMEDIATE, MAILBOX, FIFO];
        let mailbox = Policy::Prefer(vec![MAILBOX]);
        let d = decide(&mailbox, FIFO, 2, Some(&llvmpipe), || Some(0));
        assert_eq!((d.mode, d.min_image_count), (MAILBOX, 3));
        assert_eq!(d.lines[0], "[android] vulkan: swapchain present mode FIFO -> MAILBOX (driver offers IMMEDIATE, MAILBOX, FIFO)");
        assert!(d.lines[1].starts_with("[android] vulkan: minImageCount 2 -> 3 (a replacement mode needs a spare image;"));
        let d = decide(&mailbox, FIFO, 2, Some(&llvmpipe), || Some(2));
        assert_eq!((d.mode, d.min_image_count), (MAILBOX, 2));
        let d = decide(&mailbox, FIFO, 2, Some(&llvmpipe), || None);
        assert_eq!((d.mode, d.min_image_count), (MAILBOX, 2));
        // A substitution to FIFO_RELAXED never raises the count, and never
        // asks.
        let d = decide(&Policy::Prefer(vec![FIFO_RELAXED]), IMMEDIATE, 2, Some(&NVIDIA), || panic!("not asked"));
        assert_eq!((d.mode, d.min_image_count), (FIFO_RELAXED, 2));
        // Never lowered.
        let d = decide(&Policy::Prefer(vec![IMMEDIATE]), FIFO, 4, Some(&NVIDIA), || panic!("not asked"));
        assert_eq!((d.mode, d.min_image_count), (IMMEDIATE, 4));

        let d = decide(&Policy::Engine, IMMEDIATE, 3, None, || panic!("not asked"));
        assert_eq!(
            d.lines[0],
            "[android] vulkan: swapchain present mode IMMEDIATE (the engine's own choice; INTOXICATED_PRESENT_MODE=off)"
        );
        let d = decide(&Policy::Auto, FIFO, 3, None, || panic!("not asked"));
        assert_eq!(d.mode, FIFO);
        assert!(d.lines[0].ends_with("(could not ask the driver what it supports, so the engine's own choice stands)"));
    }

    #[test]
    fn modes_are_named() {
        assert_eq!(mode_name(1_000_361_000), "FIFO_LATEST_READY");
        assert_eq!(mode_name(42), "unknown");
    }
}
