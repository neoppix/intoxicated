//! Frostbite: the same client, wearing the other half of the colour wheel, on
//! two days of the year.
//!
//! On the northern hemisphere's first day of winter, and on April Fools' Day,
//! the window calls itself **Frostbite** and shows an icon whose gradient is
//! the exact per-channel inversion of Cordial's:
//!
//! ```text
//! #FF1B6B -> #00E494      #FF7A18 -> #0085E7      #B4E600 -> #4B19FF
//! ```
//!
//! Same shape, same paths, opposite palette. Cordial's mark reads as a ripe
//! mango and a cordial drink; inverted it reads as an unripe lime under frost.
//! The inversion is arithmetic rather than taste, which is why the two look
//! like twins rather than like a recolour.
//!
//! **It is a joke and it stays inside the application.** The repository never
//! rebrands: no README, no metainfo, no desktop file, no release artefact, no
//! remote asset. Someone arriving at the project on the first of April should
//! find Cordial, because a project that renames itself in its own documentation
//! is not making a joke, it is losing its name. What changes is the window in
//! front of a user who already has it installed.
//!
//! **Nothing polls.** The date is read once, on the first call, and cached for
//! the life of the process. A background timer for a passive joke would be a
//! wakeup every interval for something that changes twice a year, and this
//! project has spent enough of the last day on things that burn CPU for no
//! observable benefit. The cost is that a session running across midnight into
//! the first of April keeps its old name until it is restarted, which is the
//! correct trade for a gag.
//!
//! `CORDIAL_BRAND=frostbite` forces it on at any time, which is how you look at
//! it without setting your machine's clock to April.

use std::sync::OnceLock;

/// Which face the application is wearing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Brand {
    Cordial,
    Frostbite,
}

impl Brand {
    /// The name the window, the about dialog and the task switcher use.
    pub fn name(self) -> &'static str {
        match self {
            Brand::Cordial => "Intoxicated",
            Brand::Frostbite => "Frostbite",
        }
    }

    /// The icon name, in the freedesktop sense.
    ///
    /// **Frostbite's icon is a suffix of Cordial's id, not a name of its own,
    /// and that is a hard requirement rather than a naming preference.** Flatpak
    /// exports only files whose names begin with the application id, so
    /// `io.github.luohoa97.Frostbite` could never be exported and would never
    /// resolve for an installed build; `io.github.luohoa97.Cordial.Frostbite`
    /// is exported like any other of the app's own files.
    ///
    /// The application id itself does not change, on either day. It is the
    /// published identity -- the Flatpak ref, the update channel, the remote and
    /// the deep-link registration all key off it -- and renaming it for a joke
    /// would break installs for the sake of two days a year.
    ///
    /// Both icons ship, so this always resolves. A name that resolved to nothing
    /// would leave a blank window in the switcher on the one day nobody is
    /// watching for it, and a test asserts both files are present.
    pub fn icon(self) -> &'static str {
        match self {
            Brand::Cordial => "io.github.luohoa97.Cordial",
            Brand::Frostbite => "io.github.luohoa97.Cordial.Frostbite",
        }
    }
}

/// Whether a given month and day wears the other face.
///
/// Pure, so the two days can be tested without waiting for them. December 21st
/// is the northern hemisphere's winter solstice in most years -- it moves
/// between the 20th and the 22nd, and pinning it to the 21st is deliberate:
/// computing the true solstice needs an ephemeris, and being a day out on a
/// joke costs nothing while a dependency for it would cost a build.
pub fn frostbite_on(month: u32, day: u32) -> bool {
    matches!((month, day), (12, 21) | (4, 1))
}

/// The environment variable that forces a brand, for looking at it.
///
/// Without this, previewing a twice-a-year joke means changing the system
/// clock, which is a genuinely bad thing to ask anyone to do to their machine
/// -- it perturbs TLS validation, cron, file timestamps and the profile lock's
/// own age reporting, and one of those will be blamed on Cordial later.
///
/// `CORDIAL_BRAND=frostbite` forces it on, `=cordial` forces it off, and
/// anything else -- including the variable being absent -- lets the calendar
/// decide. Unset-means-normal is what keeps this out of the way of everybody
/// who is not looking at it.
pub const BRAND_ENV: &str = "CORDIAL_BRAND";

/// Which brand, given the override and the date. Pure, so both the override
/// and the calendar can be tested without touching either.
///
/// Separate from [`current`] because `std::env::set_var` is process-global and
/// this workspace already runs its tests in parallel threads of one process;
/// `flags.rs` keeps a mutex for exactly that hazard, and not needing one here
/// is better than sharing it.
pub fn resolve(override_value: Option<&str>, month: u32, day: u32) -> Brand {
    match override_value.map(str::trim) {
        Some(v) if v.eq_ignore_ascii_case("frostbite") => Brand::Frostbite,
        Some(v) if v.eq_ignore_ascii_case("cordial") => Brand::Cordial,
        // A value nobody recognises is not an error worth refusing a launch
        // over. It falls through to the date, which is what an unset variable
        // does, so a typo behaves like not having asked.
        _ => {
            if frostbite_on(month, day) {
                Brand::Frostbite
            } else {
                Brand::Cordial
            }
        }
    }
}

/// The brand for this process, decided once.
pub fn current() -> Brand {
    static BRAND: OnceLock<Brand> = OnceLock::new();
    *BRAND.get_or_init(|| {
        let forced = std::env::var(BRAND_ENV).ok();
        let (month, day) = local_month_day();
        resolve(forced.as_deref(), month, day)
    })
}

/// Today's month and day in local time.
///
/// Done by hand rather than with a date crate because the workspace has no
/// calendar dependency and this is the only thing that would want one. The
/// civil-from-days algorithm below is Howard Hinnant's, which is exact for the
/// proleptic Gregorian calendar and needs no table.
///
/// Local rather than UTC on purpose: the joke should land when the user's own
/// calendar says the first of April, not when Greenwich does.
fn local_month_day() -> (u32, u32) {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = (secs + local_offset_seconds()).div_euclid(86_400);
    civil_from_days(days)
}

/// Seconds east of UTC, from `localtime_r`.
///
/// `tm_gmtoff` is the offset actually in force for this instant, so it carries
/// daylight saving without a rule table.
fn local_offset_seconds() -> i64 {
    // SAFETY: `localtime_r` writes a complete `tm` into the caller's storage
    // and is the reentrant form, so nothing static is shared.
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&now, &mut tm).is_null() {
            return 0;
        }
        tm.tm_gmtoff as i64
    }
}

/// Days since 1970-01-01 to a civil month and day. Hinnant's algorithm.
fn civil_from_days(z: i64) -> (u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_days_wear_the_other_face_and_nothing_else_does() {
        assert!(frostbite_on(4, 1), "April Fools'");
        assert!(frostbite_on(12, 21), "first day of northern winter");
        // The neighbours matter more than the days themselves: an off-by-one
        // here is a client that renames itself on an ordinary Tuesday.
        for (m, d) in [(3, 31), (4, 2), (12, 20), (12, 22), (1, 1), (6, 15)] {
            assert!(!frostbite_on(m, d), "{m}-{d} must stay Cordial");
        }
    }

    #[test]
    fn both_brands_name_an_icon_that_is_actually_installed() {
        // The failure this prevents is a blank window in the task switcher on
        // the one day nobody is watching for it.
        for brand in [Brand::Cordial, Brand::Frostbite] {
            let path = format!(
                "{}/../../packaging/icons/hicolor/scalable/apps/{}.svg",
                env!("CARGO_MANIFEST_DIR"),
                brand.icon()
            );
            assert!(
                std::path::Path::new(&path).exists(),
                "{} is named but not installed at {path}",
                brand.icon()
            );
        }
    }

    /// **Both icons must be square, or the Flatpak will not export.**
    ///
    /// `flatpak build-export` refuses a non-square icon outright -- "Expected a
    /// square icon but got: 680x480" -- and it does so at the very last step,
    /// after the whole Rust build has succeeded. That is an eight-minute CI
    /// run to discover a wrong attribute in an SVG, which is exactly the kind
    /// of thing a test costing microseconds should catch instead.
    ///
    /// It happened: the Frostbite icon was cut from the wide 680x480 artboard
    /// while Cordial's uses a square 420x420 crop of the same drawing, and
    /// nothing noticed until the export. The existing test above asserts both
    /// files are *present*, which was true and insufficient.
    #[test]
    fn both_icons_are_square_or_the_flatpak_export_refuses_them() {
        for brand in [Brand::Cordial, Brand::Frostbite] {
            let path = format!(
                "{}/../../packaging/icons/hicolor/scalable/apps/{}.svg",
                env!("CARGO_MANIFEST_DIR"),
                brand.icon()
            );
            let svg = std::fs::read_to_string(&path).expect("icon is readable");
            let head = &svg[..svg.len().min(600)];
            let attr = |name: &str| -> Option<f64> {
                let at = head.find(&format!("{name}=\""))? + name.len() + 2;
                head[at..].split('"').next()?.trim().parse().ok()
            };
            let w = attr("width");
            let h = attr("height");
            assert_eq!(
                (w, h),
                (w, w),
                "{}: width and height must both be set and equal, got {w:?}x{h:?}",
                brand.icon()
            );

            // The viewBox has to be square too: a square width/height over a
            // 680x480 box still renders letterboxed, and it is the box
            // `build-export` measures.
            let vb: Vec<f64> = head
                .split("viewBox=\"")
                .nth(1)
                .expect("a viewBox")
                .split('"')
                .next()
                .unwrap()
                .split_whitespace()
                .filter_map(|n| n.parse().ok())
                .collect();
            assert_eq!(vb.len(), 4, "{}: viewBox needs four numbers", brand.icon());
            assert_eq!(
                vb[2], vb[3],
                "{}: viewBox must be square, got {}x{}",
                brand.icon(),
                vb[2],
                vb[3]
            );
        }
    }

    #[test]
    fn the_calendar_conversion_is_right_at_the_edges() {
        assert_eq!(civil_from_days(0), (1, 1)); // 1970-01-01
        assert_eq!(civil_from_days(59), (3, 1)); // 1970 is not a leap year
        assert_eq!(civil_from_days(365), (1, 1)); // 1971-01-01
        // 2024-02-29 -- a leap day, which is where a naive conversion slips.
        assert_eq!(civil_from_days(19_782), (2, 29));
    }

    #[test]
    fn the_override_wins_over_the_calendar_in_both_directions() {
        // On an ordinary day, asked for the joke.
        assert_eq!(resolve(Some("frostbite"), 6, 15), Brand::Frostbite);
        // On the day itself, asked for the ordinary thing -- which is what
        // somebody debugging a screenshot on April 1st needs.
        assert_eq!(resolve(Some("cordial"), 4, 1), Brand::Cordial);
        // Case and whitespace, because this gets typed by hand.
        assert_eq!(resolve(Some("  FrostBite "), 6, 15), Brand::Frostbite);
    }

    #[test]
    fn an_unset_or_unrecognised_value_leaves_the_calendar_in_charge() {
        assert_eq!(resolve(None, 4, 1), Brand::Frostbite);
        assert_eq!(resolve(None, 6, 15), Brand::Cordial);
        // A typo behaves like not having asked, rather than refusing to launch.
        assert_eq!(resolve(Some("frostbight"), 6, 15), Brand::Cordial);
        assert_eq!(resolve(Some(""), 12, 21), Brand::Frostbite);
    }

    #[test]
    fn the_brand_is_decided_once_and_never_polled() {
        // Two calls must agree, because the whole point is that nothing
        // re-reads the clock on a timer.
        assert_eq!(current(), current());
    }
}
