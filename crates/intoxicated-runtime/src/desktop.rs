//! The few facts the client needs from the desktop around it.

/// The window title: `Intoxicated <version> (<commit>)`.
///
/// The version is the workspace's Cargo version. The commit is stamped by
/// `build.rs` from git (with `-dirty` for a tree with uncommitted changes),
/// or from `INTOXICATED_GIT_SHA` when a packager builds without a `.git`; with
/// neither, the bracket is left off.
pub fn title() -> String {
    match option_env!("INTOXICATED_GIT_SHA") {
        Some(sha) => format!("Intoxicated {} ({sha})", env!("CARGO_PKG_VERSION")),
        None => format!("Intoxicated {}", env!("CARGO_PKG_VERSION")),
    }
}

/// Whether to tell the engine the system is in dark mode.
///
/// Decided from `GTK_THEME`, which is the one appearance setting every
/// toolkit-free X11 session can carry: a theme spelled with a `:dark` variant
/// or a `-dark` suffix (`Adwaita:dark`, `Arc-Dark`) means dark. Anything else,
/// including no `GTK_THEME` at all, means light, because a desktop that said
/// nothing has not asked for dark.
pub fn prefers_dark() -> bool {
    theme_is_dark(std::env::var("GTK_THEME").ok().as_deref())
}

fn theme_is_dark(theme: Option<&str>) -> bool {
    let Some(theme) = theme else { return false };
    let theme = theme.trim().to_ascii_lowercase();
    theme.ends_with(":dark") || theme.ends_with("-dark")
}

/// Open an http or https URL in the user's browser through `xdg-open`.
///
/// The URL comes from the engine, and so from game scripts: every other
/// scheme (`file:`, `javascript:`, custom handlers) is refused before
/// anything is spawned. The child is reaped on a thread of its own so a slow
/// browser start never holds up the caller.
pub fn open_url(url: &str) -> Result<(), String> {
    let scheme = url.split_once("://").map(|(s, _)| s.to_ascii_lowercase());
    match scheme.as_deref() {
        Some("http" | "https") => {}
        Some(other) => return Err(format!("scheme {other:?} is refused; only http and https may be opened")),
        None => return Err("not an absolute http or https URL".into()),
    }
    let mut child = std::process::Command::new("xdg-open")
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("xdg-open could not be started: {e}"))?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_title_names_the_client_and_its_version() {
        assert!(title().starts_with(&format!("Intoxicated {}", env!("CARGO_PKG_VERSION"))));
    }

    #[test]
    fn dark_is_read_from_the_theme_name() {
        assert!(theme_is_dark(Some("Adwaita:dark")));
        assert!(theme_is_dark(Some("Arc-Dark")));
        assert!(!theme_is_dark(Some("Adwaita")));
        assert!(!theme_is_dark(None));
    }

    #[test]
    fn only_web_urls_are_opened() {
        for url in ["file:///etc/passwd", "javascript:alert(1)", "mailto:a@b", "example.com"] {
            assert!(open_url(url).is_err(), "{url}");
        }
    }
}
