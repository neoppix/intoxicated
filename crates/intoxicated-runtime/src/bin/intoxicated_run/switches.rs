//! The environment switches the loader itself reads, and how each value is
//! understood. Switches read by other modules are theirs.

use std::path::PathBuf;

/// The engine's data root: `<root>/files` and `<root>/cache`.
pub const FILES_DIR: &str = "INTOXICATED_FILES_DIR";
/// The requested window size, `<w>x<h>`.
pub const RESOLUTION: &str = "INTOXICATED_RESOLUTION";
/// Which directory setters run before `initializeNativeCode`.
pub const EARLY_DIRS: &str = "INTOXICATED_EARLY_DIRS";
/// The `nativeSetRobloxChannel` argument.
pub const CHANNEL: &str = "INTOXICATED_CHANNEL";
/// The delay before the late post and retry, or `off`.
pub const LATE_POST_MS: &str = "INTOXICATED_LATE_POST_MS";
/// `off` skips the late retry.
pub const LATE_RETRY: &str = "INTOXICATED_LATE_RETRY";

/// `<root>`: `INTOXICATED_FILES_DIR` when set, else `<profile>/data`.
pub fn data_root() -> PathBuf {
    match std::env::var_os(FILES_DIR) {
        Some(root) => PathBuf::from(root),
        None => intoxicated_runtime::profile::active().join("data"),
    }
}

/// The default window size.
pub const DEFAULT_RESOLUTION: (u32, u32) = (1280, 720);

/// A requested size: unset is the default; otherwise two numbers split on
/// `x` or `X`, width 320 to 7680 and height 240 to 4320. Extra parts are
/// ignored. `Err` is anything else.
pub fn resolution(value: Option<&str>) -> Result<(u32, u32), ()> {
    let Some(value) = value else { return Ok(DEFAULT_RESOLUTION) };
    let mut parts = value.split(['x', 'X']).map(str::trim);
    let width: u32 = parts.next().and_then(|p| p.parse().ok()).ok_or(())?;
    let height: u32 = parts.next().and_then(|p| p.parse().ok()).ok_or(())?;
    if (320..=7680).contains(&width) && (240..=4320).contains(&height) {
        Ok((width, height))
    } else {
        Err(())
    }
}

/// The window size to ask for, saying so when the switch is not usable.
pub fn requested_resolution() -> (u32, u32) {
    let value = std::env::var(RESOLUTION).ok();
    resolution(value.as_deref()).unwrap_or_else(|()| {
        println!(
            "  {RESOLUTION}={:?} is not <w>x<h> within reason; using {}x{}",
            value.unwrap_or_default(),
            DEFAULT_RESOLUTION.0,
            DEFAULT_RESOLUTION.1
        );
        DEFAULT_RESOLUTION
    })
}

/// Which of the four directory setters run early.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EarlyDirs {
    pub files: bool,
    pub cache: bool,
    pub external: bool,
    pub base: bool,
}

impl EarlyDirs {
    pub fn any(self) -> bool {
        self.files || self.cache || self.external || self.base
    }
}

/// Unset: `files` and `cache`. `off` or `0`: none. `1` or `all`: all four.
/// Otherwise a comma list of `files`, `cache`, `external` and `base`, each
/// token trimmed, unknown ones ignored.
pub fn early_dirs(value: Option<&str>) -> EarlyDirs {
    match value {
        None => EarlyDirs { files: true, cache: true, ..EarlyDirs::default() },
        Some("off" | "0") => EarlyDirs::default(),
        Some("1" | "all") => EarlyDirs { files: true, cache: true, external: true, base: true },
        Some(list) => {
            let mut dirs = EarlyDirs::default();
            for token in list.split(',').map(str::trim) {
                match token {
                    "files" => dirs.files = true,
                    "cache" => dirs.cache = true,
                    "external" => dirs.external = true,
                    "base" => dirs.base = true,
                    _ => {}
                }
            }
            dirs
        }
    }
}

/// The default delay before the late post, in milliseconds: the smallest one
/// tried that did not crash, not a tuned value.
pub const LATE_POST_DEFAULT_MS: u64 = 250;

/// Unset: the default. A whole number: that many milliseconds. `off` or
/// anything else: `None`, which skips the late post and the late retry.
pub fn late_post_delay(value: Option<&str>) -> Option<u64> {
    match value {
        None => Some(LATE_POST_DEFAULT_MS),
        Some(v) => v.parse().ok(),
    }
}

/// The Roblox channel the engine is told.
pub fn channel() -> String {
    std::env::var(CHANNEL).unwrap_or_else(|_| "production".to_string())
}

/// Whether the late retry runs: anything but exactly `off`.
pub fn late_retry() -> bool {
    std::env::var(LATE_RETRY).map_or(true, |v| v != "off")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolutions() {
        assert_eq!(resolution(None), Ok((1280, 720)));
        assert_eq!(resolution(Some("1920x1080")), Ok((1920, 1080)));
        assert_eq!(resolution(Some(" 800 X 600 ")), Ok((800, 600)));
        assert_eq!(resolution(Some("1920x1080x3")), Ok((1920, 1080)));
        assert_eq!(resolution(Some("320x240")), Ok((320, 240)));
        assert_eq!(resolution(Some("7680x4320")), Ok((7680, 4320)));
        for bad in ["", "1920", "319x240", "320x239", "7681x720", "1280x4321", "axb", "-1x720", "1280*720"] {
            assert_eq!(resolution(Some(bad)), Err(()), "{bad:?}");
        }
    }

    #[test]
    fn early_directory_lists() {
        let both = EarlyDirs { files: true, cache: true, ..EarlyDirs::default() };
        let all = EarlyDirs { files: true, cache: true, external: true, base: true };
        assert_eq!(early_dirs(None), both);
        assert_eq!(early_dirs(Some("off")), EarlyDirs::default());
        assert_eq!(early_dirs(Some("0")), EarlyDirs::default());
        assert!(!early_dirs(Some("off")).any());
        assert_eq!(early_dirs(Some("1")), all);
        assert_eq!(early_dirs(Some("all")), all);
        assert_eq!(early_dirs(Some(" cache , base,bogus")), EarlyDirs { cache: true, base: true, ..EarlyDirs::default() });
        assert_eq!(early_dirs(Some("")), EarlyDirs::default());
    }

    #[test]
    fn late_post_delays() {
        assert_eq!(late_post_delay(None), Some(250));
        assert_eq!(late_post_delay(Some("0")), Some(0));
        assert_eq!(late_post_delay(Some("1000")), Some(1000));
        assert_eq!(late_post_delay(Some("off")), None);
        assert_eq!(late_post_delay(Some("soon")), None);
        assert_eq!(late_post_delay(Some("-5")), None);
    }
}
