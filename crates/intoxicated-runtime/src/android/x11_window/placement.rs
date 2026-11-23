//! Where the window opens, how big, and what it is called. Pure: the X side
//! (the Xinerama query, the prints) is the caller's.

use super::xlib::Head;

/// The window's first position and size, and what the window manager is
/// asked for once it is mapped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Placement {
    pub(super) x: i32,
    pub(super) y: i32,
    pub(super) width: i32,
    pub(super) height: i32,
    pub(super) fullscreen: bool,
    /// The Xinerama head the window was placed on, for
    /// `_NET_WM_FULLSCREEN_MONITORS`. `None` unless a monitor was asked for
    /// and Xinerama answered.
    pub(super) monitor: Option<u32>,
}

/// The three placement switches, as read from the environment.
#[derive(Debug, Clone, Default)]
pub(super) struct PlacementRequest {
    /// `INTOXICATED_WINDOW_POS`.
    pub(super) position: Option<String>,
    /// `INTOXICATED_MONITOR`.
    pub(super) monitor: Option<String>,
    /// `INTOXICATED_FULLSCREEN`, present with any value.
    pub(super) fullscreen: bool,
}

impl PlacementRequest {
    pub(super) fn from_env() -> Self {
        PlacementRequest {
            position: std::env::var("INTOXICATED_WINDOW_POS").ok(),
            monitor: std::env::var("INTOXICATED_MONITOR").ok(),
            fullscreen: std::env::var_os("INTOXICATED_FULLSCREEN").is_some(),
        }
    }
}

/// `<x>,<y>`: two integers, each trimmed; anything after a third comma is
/// ignored.
pub(super) fn parse_position(v: &str) -> Option<(i32, i32)> {
    let mut parts = v.split(',');
    let x = parts.next()?.trim().parse().ok()?;
    let y = parts.next()?.trim().parse().ok()?;
    Some((x, y))
}

/// Decide the placement for a `width` x `height` request.
///
/// `heads` is asked only when a monitor index was given and parses; `Err`
/// means libXinerama could not be opened. Every warning the decision makes
/// is pushed to `warnings`, worded as it is printed.
pub(super) fn place(
    width: i32,
    height: i32,
    request: &PlacementRequest,
    heads: impl FnOnce() -> Result<Vec<Head>, ()>,
    warnings: &mut Vec<String>,
) -> Placement {
    let fullscreen = request.fullscreen;
    let plain = Placement { x: 0, y: 0, width, height, fullscreen, monitor: None };

    if let Some(v) = &request.position {
        match parse_position(v) {
            Some((x, y)) => return Placement { x, y, ..plain },
            None => warnings.push(format!("[android] INTOXICATED_WINDOW_POS={v:?} is not <x>,<y>; ignoring")),
        }
    }

    let Some(v) = &request.monitor else { return plain };
    let Ok(n) = v.trim().parse::<u32>() else {
        warnings.push("[android] INTOXICATED_MONITOR must be a number; ignoring".into());
        return plain;
    };
    let Ok(heads) = heads() else {
        warnings.push("[android] INTOXICATED_MONITOR needs libXinerama; ignoring".into());
        return plain;
    };
    if heads.is_empty() {
        return plain;
    }
    let index = if (n as usize) < heads.len() {
        n
    } else {
        warnings.push(format!(
            "[android] INTOXICATED_MONITOR={n} but only {} monitor(s); using the first",
            heads.len()
        ));
        0
    };
    let head = heads[index as usize];
    if fullscreen {
        return Placement {
            x: head.x,
            y: head.y,
            width: head.width,
            height: head.height,
            fullscreen,
            monitor: Some(index),
        };
    }
    Placement {
        x: head.x + ((head.width - width) / 2).max(0),
        y: head.y + ((head.height - height) / 2).max(0),
        monitor: Some(index),
        ..plain
    }
}

/// The text for `WM_NAME`, which is Latin-1 `STRING`: every character
/// outside ASCII becomes one `-`, so nothing is encoded twice.
pub(super) fn ascii_title(title: &str) -> String {
    title.chars().map(|c| if c.is_ascii() { c } else { '-' }).collect()
}
