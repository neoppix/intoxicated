//! The machine's power state, read from FreeBSD and told to the engine the
//! way Android's `BatteryManager` would tell it.
//!
//! Written from a behavioural spec, acpi(4), the kernel's
//! `dev/acpica/acpi_battery.c`, `acpi_acad.c` and `acpiio.h`, and Android's
//! public `BatteryManager` constants.
//!
//! # Where the facts come from
//!
//! * `hw.acpi.acline`: 1 on AC, 0 off. It exists only when an ACPI AC adapter
//!   attached.
//! * `hw.acpi.battery.{units,life,time,rate,state}`: the kernel's aggregate
//!   over every ACPI battery, -1 for unknown; `state` is 1 discharging, 2
//!   charging, 4 critical, 7 not present. The node appears when the first
//!   battery registers.
//! * For unit 0, its voltage and type string through `/dev/acpi`
//!   (`ACPIIO_BATT_GET_BST`, `ACPIIO_BATT_GET_BIX`), read only while a battery
//!   is present. Any failure leaves both unknown.
//! * The SMBIOS chassis type (`smbios.chassis.type` in kenv) and whether ACPI
//!   is attached at all, used only when there is no battery node; see below.
//!
//! `apm(8)` is never read: with no AC adapter the kernel's APM emulation
//! reports "on-line" and a charging battery as a guess, on a machine with no
//! battery. `/sys/class/power_supply` is Linux and does not exist here.
//!
//! # A desktop
//!
//! A missing sysctl node is not a value. But a machine whose firmware is
//! running ACPI and lists no battery, and whose SMBIOS chassis says it is a
//! desktop, tower or server, has no battery, and a machine with no battery is
//! on mains power. That case alone is reported as "no battery, on AC"
//! (`present` false, `plugged` AC). With either fact missing, or a portable
//! chassis, nothing is invented: the engine gets nulls, exactly as before.
//!
//! # When the engine hears
//!
//! Once at startup ([`start`]). Then, only if one of the sysctl nodes existed
//! at startup (so a desktop pays nothing), a thread re-reads every 15 seconds
//! and hands a changed reading to the looper ([`tick`]), which calls the
//! engine's natives on its own thread like every other periodic engine call:
//! `reportBatteryStatus` when any field changed, `reportBatteryStateChanged`
//! when (status, plugged) changed. The sysctls evaluate ACPI methods under
//! Giant, which is why they are not read on the looper.

use std::ffi::{c_void, CStr};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use intoxicated_linker_sys::game_activity::{self, BatteryStatusFields};

/// Android `BatteryManager.BATTERY_STATUS_*`.
pub const STATUS_UNKNOWN: i32 = 1;
pub const STATUS_CHARGING: i32 = 2;
pub const STATUS_DISCHARGING: i32 = 3;
pub const STATUS_NOT_CHARGING: i32 = 4;
pub const STATUS_FULL: i32 = 5;
/// Android `BatteryManager.BATTERY_PLUGGED_AC`.
pub const PLUGGED_AC: i32 = 1;

/// `hw.acpi.battery.state` bits, from `acpiio.h`.
const STATE_DISCHARGING: i32 = 0x1;
const STATE_CHARGING: i32 = 0x2;
const STATE_NOT_PRESENT: i32 = 0x7;

/// How often a machine with a battery or an AC adapter is read again.
const POLL: Duration = Duration::from_secs(15);

/// The kernel's battery node, every value as read; `None` where the oid
/// itself is missing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BatteryNode {
    pub units: Option<i32>,
    pub life: Option<i32>,
    pub time: Option<i32>,
    pub rate: Option<i32>,
    pub state: Option<i32>,
}

/// One reading of everything the report is made from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reading {
    /// `hw.acpi.acline`.
    pub acline: Option<i32>,
    /// `hw.acpi.battery.*`, `None` when no oid under it exists.
    pub battery: Option<BatteryNode>,
    /// Unit 0's present voltage in mV, from `ACPIIO_BATT_GET_BST`.
    pub voltage_mv: Option<i32>,
    /// Unit 0's type string, from `ACPIIO_BATT_GET_BIX`.
    pub technology: Option<String>,
    /// Whether the ACPI driver is attached (`hw.acpi.supported_sleep_state`).
    pub acpi: bool,
    /// `smbios.chassis.type`, as FreeBSD's loader spells it.
    pub chassis: Option<String>,
}

impl Reading {
    /// Whether anything here can change while the client runs.
    fn has_nodes(&self) -> bool {
        self.acline.is_some() || self.battery.is_some()
    }
}

/// What the engine is told for one reading.
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    /// `reportBatteryStateChanged(status, plugged)`, made only for a present
    /// battery.
    pub state_changed: Option<(i32, i32)>,
    /// `reportBatteryStatus`.
    pub status: BatteryStatusFields,
}

/// SMBIOS chassis types that carry no battery of their own, in the spelling
/// of FreeBSD's `stand/libsa/smbios.c`.
fn chassis_has_no_battery(chassis: &str) -> bool {
    matches!(
        chassis,
        "Desktop"
            | "Low Profile Desktop"
            | "Pizza Box"
            | "Mini Tower"
            | "Tower"
            | "Lunch Box"
            | "Space-saving"
            | "Main Server Chassis"
            | "Rack Mount Chassis"
            | "Sealed-case PC"
            | "Mini PC"
    )
}

/// The engine's two calls for `r`. Pure, so every row of the mapping is a
/// test.
pub fn report(r: &Reading) -> Report {
    let plugged = match r.acline {
        Some(1) => Some(PLUGGED_AC),
        Some(0) => Some(0),
        _ => None,
    };
    let mut status = BatteryStatusFields { plugged, ..Default::default() };
    let Some(node) = r.battery else {
        // No battery node. Only a desktop running ACPI is known to have no
        // battery; see the module doc.
        if r.acpi && r.chassis.as_deref().is_some_and(chassis_has_no_battery) {
            status.present = Some(false);
            status.plugged = Some(plugged.unwrap_or(PLUGGED_AC));
        }
        return Report { state_changed: None, status };
    };
    if node.units == Some(0) || node.state == Some(STATE_NOT_PRESENT) {
        status.present = Some(false);
        return Report { state_changed: None, status };
    }
    let code = status_code(node.state, node.life, r.acline);
    status.present = Some(true);
    status.percentage = node.life.filter(|l| (0..=100).contains(l));
    status.status = Some(code);
    // mW to microwatts.
    status.power_now_uw = node.rate.filter(|&p| p >= 0).map(|p| p.saturating_mul(1000));
    status.voltage_mv = r.voltage_mv;
    status.technology = r.technology.clone();
    Report { state_changed: Some((code, plugged.unwrap_or(0))), status }
}

/// `BATTERY_STATUS_*` from the kernel's state bits, the remaining capacity
/// and the AC line. The kernel clears charging on a battery that is also
/// discharging; discharging wins here too. Critical changes nothing.
/// INFERRED: neither bit set on AC means full at 100% and not charging below
/// it; a capacity the kernel does not know gives unknown.
fn status_code(state: Option<i32>, life: Option<i32>, acline: Option<i32>) -> i32 {
    let Some(state) = state.filter(|&s| s >= 0) else {
        return STATUS_UNKNOWN;
    };
    if state & STATE_DISCHARGING != 0 {
        STATUS_DISCHARGING
    } else if state & STATE_CHARGING != 0 {
        STATUS_CHARGING
    } else if acline == Some(1) {
        match life {
            Some(100) => STATUS_FULL,
            Some(0..=99) => STATUS_NOT_CHARGING,
            _ => STATUS_UNKNOWN,
        }
    } else {
        STATUS_UNKNOWN
    }
}

/// What `r` says, for the log.
pub fn describe(r: &Reading) -> String {
    let n = |v: Option<i32>| v.map_or("absent".to_string(), |v| v.to_string());
    let acline = match r.acline {
        Some(v) => format!("hw.acpi.acline={v}"),
        None => "no hw.acpi.acline".to_string(),
    };
    match r.battery {
        Some(b) => format!(
            "{acline}, hw.acpi.battery units={} life={} time={} rate={} state={}",
            n(b.units),
            n(b.life),
            n(b.time),
            n(b.rate),
            n(b.state)
        ),
        None => format!(
            "{acline}, no hw.acpi.battery; ACPI {}, SMBIOS chassis {}",
            if r.acpi { "attached" } else { "not attached" },
            r.chassis.as_deref().map_or("unknown".to_string(), |c| format!("{c:?}")),
        ),
    }
}

// ------------------------------------------------------------- reading

fn sysctl_int(name: &CStr) -> Option<i32> {
    let mut v: libc::c_int = 0;
    let mut len = std::mem::size_of::<libc::c_int>();
    // SAFETY: a NUL-terminated name and an int-sized buffer with its length.
    let rc = unsafe {
        libc::sysctlbyname(name.as_ptr(), (&mut v as *mut libc::c_int).cast(), &mut len, std::ptr::null(), 0)
    };
    (rc == 0 && len == std::mem::size_of::<libc::c_int>()).then_some(v)
}

fn sysctl_exists(name: &CStr) -> bool {
    let mut len = 0usize;
    // SAFETY: a size query: no buffer, only the length is written.
    unsafe { libc::sysctlbyname(name.as_ptr(), std::ptr::null_mut(), &mut len, std::ptr::null(), 0) == 0 }
}

fn kenv_get(name: &CStr) -> Option<String> {
    let mut buf = [0 as libc::c_char; 128];
    // SAFETY: KENV_GET writes at most `len` bytes, NUL-terminated, into `buf`.
    let n = unsafe { libc::kenv(libc::KENV_GET, name.as_ptr(), buf.as_mut_ptr(), buf.len() as libc::c_int) };
    if n < 0 {
        return None;
    }
    // SAFETY: the kernel NUL-terminates what it copies out.
    let s = unsafe { CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// `_IOWR(group, num, len)` from `sys/ioccom.h`.
const fn iowr(group: u8, num: u8, len: usize) -> libc::c_ulong {
    (0xC000_0000u32 | (((len as u32) & 0x1fff) << 16) | ((group as u32) << 8) | num as u32) as libc::c_ulong
}

/// `union acpi_battery_ioctl_arg` is 256 bytes (printed from `acpiio.h`).
const BATTERY_ARG_LEN: usize = 256;
const ACPIIO_BATT_GET_BIX: libc::c_ulong = iowr(b'B', 0x10, BATTERY_ARG_LEN);
const ACPIIO_BATT_GET_BST: libc::c_ulong = iowr(b'B', 0x11, BATTERY_ARG_LEN);
/// `struct acpi_bst.volt` and `struct acpi_bix.type`, by offset.
const BST_VOLT_AT: usize = 12;
const BIX_TYPE_AT: usize = 100;
const ACPI_STRING_LEN: usize = 32;
const ACPI_BATT_UNKNOWN: u32 = 0xffff_ffff;

/// Unit 0's voltage and type string. Opened read-only, which the battery
/// ioctls allow.
fn unit0_details() -> (Option<i32>, Option<String>) {
    // SAFETY: a NUL-terminated path.
    let fd = unsafe { libc::open(c"/dev/acpi".as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
    if fd < 0 {
        return (None, None);
    }
    let ask = |request: libc::c_ulong| -> Option<[u8; BATTERY_ARG_LEN]> {
        // Zeroed, so `unit`, the union's first member, asks for unit 0.
        let mut arg = [0u8; BATTERY_ARG_LEN];
        // SAFETY: `arg` is exactly the size the request encodes.
        let rc = unsafe { libc::ioctl(fd, request, arg.as_mut_ptr()) };
        (rc == 0).then_some(arg)
    };
    let voltage = ask(ACPIIO_BATT_GET_BST)
        .map(|a| u32::from_ne_bytes(a[BST_VOLT_AT..BST_VOLT_AT + 4].try_into().unwrap_or([0xff; 4])))
        .filter(|&v| v != ACPI_BATT_UNKNOWN && v <= i32::MAX as u32)
        .map(|v| v as i32);
    let technology = ask(ACPIIO_BATT_GET_BIX).and_then(|a| {
        let raw = &a[BIX_TYPE_AT..BIX_TYPE_AT + ACPI_STRING_LEN];
        let end = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
        let s = String::from_utf8_lossy(&raw[..end]).trim().to_string();
        (!s.is_empty()).then_some(s)
    });
    // SAFETY: our own descriptor.
    unsafe { libc::close(fd) };
    (voltage, technology)
}

/// Read everything, now, on the calling thread.
pub fn read() -> Reading {
    let node = BatteryNode {
        units: sysctl_int(c"hw.acpi.battery.units"),
        life: sysctl_int(c"hw.acpi.battery.life"),
        time: sysctl_int(c"hw.acpi.battery.time"),
        rate: sysctl_int(c"hw.acpi.battery.rate"),
        state: sysctl_int(c"hw.acpi.battery.state"),
    };
    let battery = (node != BatteryNode::default()).then_some(node);
    let mut r = Reading { acline: sysctl_int(c"hw.acpi.acline"), battery, ..Default::default() };
    let report = report(&r);
    if report.status.present == Some(true) {
        (r.voltage_mv, r.technology) = unit0_details();
    }
    if r.battery.is_none() {
        r.acpi = sysctl_exists(c"hw.acpi.supported_sleep_state");
        r.chassis = kenv_get(c"smbios.chassis.type");
    }
    r
}

// ------------------------------------------------------------ reporting

/// The two natives' addresses, kept for the looper's calls.
static NATIVES: OnceLock<(usize, usize)> = OnceLock::new();
/// The last report the engine was given.
static LAST: Mutex<Option<Report>> = Mutex::new(None);
/// A changed reading from the poller, waiting for the looper.
static PENDING: Mutex<Option<Reading>> = Mutex::new(None);
static HAS_PENDING: AtomicBool = AtomicBool::new(false);

/// Tell the engine, with the log lines that say what it was told. Only the
/// parts that differ from `previous` are sent.
fn deliver(state_changed_native: *mut c_void, status_native: *mut c_void, now: &Report, previous: Option<&Report>) {
    let before = previous.and_then(|p| p.state_changed);
    match now.state_changed {
        Some((status, plugged)) if before != now.state_changed => {
            match game_activity::report_battery_state_changed(state_changed_native, status, plugged) {
                Ok(()) => println!("  battery: reportBatteryStateChanged({status}, {plugged})"),
                Err(e) => println!("  battery: reportBatteryStateChanged failed: {e}"),
            }
        }
        Some(_) => {}
        None if previous.is_none() => println!("  battery: no present battery; reportBatteryStateChanged skipped"),
        None => {}
    }
    if previous.is_none_or(|p| p.status != now.status) {
        match game_activity::report_battery_status(status_native, &now.status) {
            Ok(()) => println!("  battery: reportBatteryStatus {:?}", now.status),
            Err(e) => println!("  battery: reportBatteryStatus failed: {e}"),
        }
    }
}

/// The startup report, from `intoxicated-run` once both natives resolved, on the
/// thread that later runs the looper.
pub fn start(state_changed_native: *mut c_void, status_native: *mut c_void) {
    let reading = read();
    let now = report(&reading);
    let verdict = match (now.status.present, now.status.plugged) {
        (Some(false), Some(PLUGGED_AC)) => ": no battery, on AC",
        (Some(false), _) => ": no battery present",
        _ => "",
    };
    println!("  battery: {}{verdict}", describe(&reading));
    deliver(state_changed_native, status_native, &now, None);
    *LAST.lock().unwrap_or_else(|e| e.into_inner()) = Some(now);
    if !reading.has_nodes() {
        return;
    }
    let _ = NATIVES.set((state_changed_native as usize, status_native as usize));
    let spawned = std::thread::Builder::new().name("intoxicated-battery".into()).spawn(move || {
        let mut last = reading;
        loop {
            std::thread::sleep(POLL);
            let next = read();
            if next != last {
                *PENDING.lock().unwrap_or_else(|e| e.into_inner()) = Some(next.clone());
                HAS_PENDING.store(true, Relaxed);
                last = next;
            }
        }
    });
    match spawned {
        Ok(_) => println!("  battery: read again every {} s; the engine hears of changes", POLL.as_secs()),
        Err(e) => println!("  battery: could not start the re-reading thread ({e}); reported once"),
    }
}

/// From the looper, every pass. One relaxed load unless the poller handed
/// over a changed reading.
pub fn tick() {
    if !HAS_PENDING.load(Relaxed) || !HAS_PENDING.swap(false, Relaxed) {
        return;
    }
    let Some(reading) = PENDING.lock().unwrap_or_else(|e| e.into_inner()).take() else {
        return;
    };
    let Some(&(state_changed_native, status_native)) = NATIVES.get() else {
        return;
    };
    let now = report(&reading);
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if last.as_ref() == Some(&now) {
        return;
    }
    println!("  battery: {}", describe(&reading));
    deliver(state_changed_native as *mut c_void, status_native as *mut c_void, &now, last.as_ref());
    *last = Some(now);
}

// ------------------------------------------------------------- the sysfs view

/// Android `BatteryManager.BATTERY_PLUGGED_USB` and `_WIRELESS`.
const PLUGGED_USB: i32 = 2;
const PLUGGED_WIRELESS: i32 = 4;

/// What the engine was told last, or before it was told anything, one
/// reading taken for the purpose and kept. sysfs reads come from the
/// engine's threads, and the sysctls behind a fresh reading evaluate ACPI
/// methods under Giant, so they are not read again per file.
fn current_report() -> Report {
    if let Some(r) = LAST.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        return r;
    }
    static EARLY: OnceLock<Report> = OnceLock::new();
    EARLY.get_or_init(|| report(&read())).clone()
}

/// The word Linux's `status` attribute uses for a `BATTERY_STATUS_*` code.
fn status_word(code: i32) -> &'static str {
    match code {
        STATUS_CHARGING => "Charging",
        STATUS_DISCHARGING => "Discharging",
        STATUS_NOT_CHARGING => "Not charging",
        STATUS_FULL => "Full",
        _ => "Unknown",
    }
}

/// The text of `/sys/class/power_supply/<supply>/<attribute>` for `r`, so the
/// synthetic sysfs (`native/system_paths.cpp`) tells the engine what
/// `BatteryManager` does: a desktop has no battery (`battery/present` is 0,
/// its other attributes do not exist) and is on AC (`ac/online` is 1); a
/// laptop reports its real capacity and state. `None` is a node this machine
/// would not have, and the open fails with ENOENT.
pub fn power_supply_text(r: &Report, path: &str) -> Option<String> {
    let (supply, attr) = path.strip_prefix("/sys/class/power_supply/")?.split_once('/')?;
    if attr.contains('/') {
        return None;
    }
    let s = &r.status;
    let supply = supply.to_ascii_lowercase();
    let value = if supply.starts_with("bat") {
        match (attr, s.present) {
            ("present", Some(p)) => Some(if p { "1" } else { "0" }.to_string()),
            ("type", Some(_)) => Some("Battery".to_string()),
            (_, Some(true)) => match attr {
                "capacity" => s.percentage.map(|p| p.to_string()),
                "status" => s.status.map(|c| status_word(c).to_string()),
                "voltage_now" => s.voltage_mv.map(|v| (i64::from(v) * 1000).to_string()),
                "power_now" => s.power_now_uw.map(|p| p.to_string()),
                "technology" => s.technology.clone(),
                _ => None,
            },
            _ => None,
        }
    } else {
        let (kind, name) = match supply.as_str() {
            "ac" | "mains" => (PLUGGED_AC, "Mains"),
            "usb" => (PLUGGED_USB, "USB"),
            "wireless" => (PLUGGED_WIRELESS, "Wireless"),
            _ => return None,
        };
        match attr {
            "online" => s.plugged.map(|p| if p == kind { "1" } else { "0" }.to_string()),
            "type" if s.plugged.is_some() => Some(name.to_string()),
            _ => None,
        }
    };
    value.map(|v| v + "\n")
}

/// [`power_supply_text`] for C: the text and its length into `buf` (NUL
/// terminated, truncated to `cap - 1`), or -1 for a node that does not exist.
/// `bionic::system_path_overrides` hands it to `native/system_paths.cpp`.
pub extern "C" fn intoxicated_power_supply_text(path: *const std::ffi::c_char, buf: *mut std::ffi::c_char, cap: usize) -> isize {
    if path.is_null() || buf.is_null() || cap == 0 {
        return -1;
    }
    // SAFETY: a NUL-terminated path from the caller.
    let path = unsafe { CStr::from_ptr(path) }.to_string_lossy();
    let Some(text) = power_supply_text(&current_report(), &path) else {
        return -1;
    };
    let n = text.len().min(cap - 1);
    // SAFETY: `buf` holds `cap` bytes.
    unsafe {
        std::ptr::copy_nonoverlapping(text.as_ptr(), buf.cast::<u8>(), n);
        *buf.add(n) = 0;
    }
    n as isize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sysfs_tells_what_battery_manager_tells() {
        let at = |r: &Reading, node: &str| power_supply_text(&report(r), &format!("/sys/class/power_supply/{node}"));
        // This desktop: no battery, on AC.
        let desk = Reading { acpi: true, chassis: Some("Desktop".into()), ..Default::default() };
        assert_eq!(at(&desk, "battery/present").as_deref(), Some("0\n"));
        assert_eq!(at(&desk, "battery/capacity"), None, "a desktop has no capacity to read");
        assert_eq!(at(&desk, "battery/status"), None);
        assert_eq!(at(&desk, "ac/online").as_deref(), Some("1\n"));
        assert_eq!(at(&desk, "usb/online").as_deref(), Some("0\n"));
        // A laptop on battery.
        let lap = Reading { voltage_mv: Some(11_900), ..reading(Some(0), node(1, 55, 120, 9000, 1)) };
        assert_eq!(at(&lap, "battery/present").as_deref(), Some("1\n"));
        assert_eq!(at(&lap, "battery/capacity").as_deref(), Some("55\n"));
        assert_eq!(at(&lap, "BAT0/status").as_deref(), Some("Discharging\n"));
        assert_eq!(at(&lap, "battery/voltage_now").as_deref(), Some("11900000\n"));
        assert_eq!(at(&lap, "ac/online").as_deref(), Some("0\n"));
        // Charging and full on AC.
        assert_eq!(at(&reading(Some(1), node(1, 73, -1, 15000, 2)), "battery/status").as_deref(), Some("Charging\n"));
        assert_eq!(at(&reading(Some(1), node(1, 100, -1, 0, 0)), "battery/status").as_deref(), Some("Full\n"));
        // Nothing known, nothing invented.
        let blank = Reading::default();
        assert_eq!(at(&blank, "battery/present"), None);
        assert_eq!(at(&blank, "ac/online"), None);
        assert_eq!(power_supply_text(&report(&desk), "/sys/class/power_supply/battery"), None);
    }

    #[test]
    fn the_c_entry_writes_text_or_answers_absent() {
        let mut buf = [0 as std::ffi::c_char; 16];
        let n = intoxicated_power_supply_text(c"/sys/class/power_supply/nonesuch/online".as_ptr(), buf.as_mut_ptr(), buf.len());
        assert_eq!(n, -1);
        // Whatever this machine is, `battery/present` agrees with the report.
        let n = intoxicated_power_supply_text(c"/sys/class/power_supply/battery/present".as_ptr(), buf.as_mut_ptr(), buf.len());
        match current_report().status.present {
            Some(p) => {
                assert_eq!(n, 2);
                assert_eq!(buf[0] as u8, if p { b'1' } else { b'0' });
            }
            None => assert_eq!(n, -1),
        }
    }

    fn node(units: i32, life: i32, time: i32, rate: i32, state: i32) -> Option<BatteryNode> {
        Some(BatteryNode {
            units: Some(units),
            life: Some(life),
            time: Some(time),
            rate: Some(rate),
            state: Some(state),
        })
    }

    fn reading(acline: Option<i32>, battery: Option<BatteryNode>) -> Reading {
        Reading { acline, battery, ..Default::default() }
    }

    fn fields(
        present: Option<bool>,
        percentage: Option<i32>,
        status: Option<i32>,
        plugged: Option<i32>,
        power: Option<i32>,
    ) -> BatteryStatusFields {
        BatteryStatusFields { present, percentage, status, plugged, power_now_uw: power, ..Default::default() }
    }

    #[test]
    fn every_row_of_the_mapping() {
        let rows: [(Reading, Option<(i32, i32)>, BatteryStatusFields); 11] = [
            // No AC adapter, no battery node, nothing else known: nothing.
            (reading(None, None), None, BatteryStatusFields::default()),
            (reading(Some(1), None), None, fields(None, None, None, Some(1), None)),
            // Laptop charging.
            (
                reading(Some(1), node(1, 73, -1, 15000, 2)),
                Some((2, 1)),
                fields(Some(true), Some(73), Some(2), Some(1), Some(15_000_000)),
            ),
            // Laptop discharging.
            (
                reading(Some(0), node(1, 41, 95, 9000, 1)),
                Some((3, 0)),
                fields(Some(true), Some(41), Some(3), Some(0), Some(9_000_000)),
            ),
            // Full on AC.
            (
                reading(Some(1), node(1, 100, -1, 0, 0)),
                Some((5, 1)),
                fields(Some(true), Some(100), Some(5), Some(1), Some(0)),
            ),
            // On AC, not charging.
            (
                reading(Some(1), node(1, 80, -1, -1, 0)),
                Some((4, 1)),
                fields(Some(true), Some(80), Some(4), Some(1), None),
            ),
            // No AC adapter node: the state call says unplugged, the status leaves it null.
            (reading(None, node(1, 55, 60, -1, 1)), Some((3, 0)), fields(Some(true), Some(55), Some(3), None, None)),
            // Critical does not change the status.
            (
                reading(Some(0), node(1, 5, 10, 8000, 5)),
                Some((3, 0)),
                fields(Some(true), Some(5), Some(3), Some(0), Some(8_000_000)),
            ),
            // Battery slot empty.
            (reading(Some(1), node(1, -1, -1, -1, 7)), None, fields(Some(false), None, None, Some(1), None)),
            // Two batteries, one discharging and one charging: discharging wins.
            (
                reading(Some(1), node(2, 60, -1, -1, 3)),
                Some((3, 1)),
                fields(Some(true), Some(60), Some(3), Some(1), None),
            ),
            // A capacity out of range is not passed on.
            (reading(Some(1), node(1, 142, -1, -1, 2)), Some((2, 1)), fields(Some(true), None, Some(2), Some(1), None)),
        ];
        for (i, (r, state_changed, status)) in rows.into_iter().enumerate() {
            let got = report(&r);
            assert_eq!(got.state_changed, state_changed, "row {i}: {r:?}");
            assert_eq!(got.status, status, "row {i}: {r:?}");
        }
    }

    #[test]
    fn this_desktop_is_no_battery_on_ac_and_never_calls_the_state_change() {
        // As measured here: no acline, no battery node, ACPI attached,
        // chassis "Desktop".
        let r = Reading { acpi: true, chassis: Some("Desktop".into()), ..Default::default() };
        let got = report(&r);
        assert_eq!(got.state_changed, None);
        assert_eq!(
            got.status,
            BatteryStatusFields { present: Some(false), plugged: Some(PLUGGED_AC), ..Default::default() }
        );
        assert!(!r.has_nodes(), "a desktop is not polled");
    }

    #[test]
    fn a_missing_node_is_not_turned_into_a_value_without_both_facts() {
        for r in [
            Reading { acpi: false, chassis: Some("Desktop".into()), ..Default::default() },
            Reading { acpi: true, chassis: None, ..Default::default() },
            Reading { acpi: true, chassis: Some("Notebook".into()), ..Default::default() },
            Reading { acpi: true, chassis: Some("Default string".into()), ..Default::default() },
        ] {
            assert_eq!(report(&r), Report { state_changed: None, status: BatteryStatusFields::default() }, "{r:?}");
        }
        // An AC adapter on a desktop is believed over the assumption.
        let r = Reading { acline: Some(0), acpi: true, chassis: Some("Tower".into()), ..Default::default() };
        assert_eq!(
            report(&r).status,
            BatteryStatusFields { present: Some(false), plugged: Some(0), ..Default::default() }
        );
    }

    #[test]
    fn units_zero_is_no_battery_and_the_ioctl_details_ride_along_when_present() {
        let r = reading(Some(1), Some(BatteryNode { units: Some(0), ..Default::default() }));
        assert_eq!(report(&r).status, fields(Some(false), None, None, Some(1), None));
        let r = Reading {
            voltage_mv: Some(12_400),
            technology: Some("LION".into()),
            ..reading(Some(0), node(1, 50, 120, 7000, 1))
        };
        let s = report(&r).status;
        assert_eq!((s.voltage_mv, s.technology.as_deref()), (Some(12_400), Some("LION")));
        assert_eq!((s.health, s.temperature_c, s.current_now_ua, s.charge_counter_uah), (None, None, None, None));
    }

    #[test]
    fn status_without_a_known_state_or_capacity_is_unknown() {
        assert_eq!(status_code(None, Some(50), Some(1)), STATUS_UNKNOWN);
        assert_eq!(status_code(Some(0), Some(-1), Some(1)), STATUS_UNKNOWN);
        assert_eq!(status_code(Some(0), Some(80), Some(0)), STATUS_UNKNOWN);
        assert_eq!(status_code(Some(0), Some(80), None), STATUS_UNKNOWN);
    }

    #[test]
    fn the_ioctl_numbers_match_acpiio_h() {
        // Printed from <dev/acpica/acpiio.h> with the system compiler.
        assert_eq!(ACPIIO_BATT_GET_BIX, 0xc100_4210);
        assert_eq!(ACPIIO_BATT_GET_BST, 0xc100_4211);
    }

    #[test]
    fn reading_this_machine_does_not_invent_a_battery() {
        // Whatever machine runs the tests: a battery is reported only where
        // the kernel has a battery node.
        let r = read();
        let got = report(&r);
        if r.battery.is_none() {
            assert_ne!(got.status.present, Some(true));
            assert_eq!(got.state_changed, None);
        }
        assert!(!describe(&r).is_empty());
    }
}
