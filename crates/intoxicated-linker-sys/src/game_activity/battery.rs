//! The two battery reports (`native/battery_status.cpp`).

use std::ffi::{c_char, c_int, c_void, CString};

use crate::call::{c_text, status};
use crate::ffi;

/// `NativeGLInterface.reportBatteryStateChanged(II)V`, with Android's raw
/// `BatteryManager` status and plug values.
pub fn report_battery_state_changed(native: *mut c_void, status_code: i32, plugged: i32) -> Result<(), String> {
    // SAFETY: plain values and the helper's buffer.
    status(|err, len| unsafe { ffi::intoxicated_report_battery_state_changed(native, status_code, plugged, err, len) })
}

/// One battery reading, every field optional.
///
/// `None` reaches Java as a null field, which is how the engine's
/// `BatteryStatus` says "not known"; a made-up zero would be a reading of a
/// battery at 0%, 0 mV.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BatteryStatusFields {
    pub present: Option<bool>,
    pub percentage: Option<i32>,
    pub status: Option<i32>,
    pub health: Option<i32>,
    pub voltage_mv: Option<i32>,
    pub current_now_ua: Option<i32>,
    pub current_avg_ua: Option<i32>,
    pub charge_counter_uah: Option<i32>,
    pub power_now_uw: Option<i32>,
    pub technology: Option<String>,
    pub temperature_c: Option<f32>,
    pub plugged: Option<i32>,
}

/// `IntoxicatedBatteryWire` as the C side declares it: a has-flag before every
/// value, 0 meaning absent. 104 bytes, 8-aligned because of `technology`.
#[repr(C)]
pub(crate) struct BatteryWire {
    has_present: c_int,
    present: c_int,
    has_percentage: c_int,
    percentage: c_int,
    has_status: c_int,
    status: c_int,
    has_health: c_int,
    health: c_int,
    has_voltage_mv: c_int,
    voltage_mv: c_int,
    has_current_now_ua: c_int,
    current_now_ua: c_int,
    has_current_avg_ua: c_int,
    current_avg_ua: c_int,
    has_charge_counter_uah: c_int,
    charge_counter_uah: c_int,
    has_power_now_uw: c_int,
    power_now_uw: c_int,
    has_technology: c_int,
    technology: *const c_char,
    has_temperature_c: c_int,
    temperature_c: f32,
    has_plugged: c_int,
    plugged: c_int,
}

/// A has-flag and a value, zero for both when absent.
fn slot<T: Default>(value: Option<T>) -> (c_int, T) {
    match value {
        Some(v) => (1, v),
        None => (0, T::default()),
    }
}

impl BatteryWire {
    /// The wire form of `fields`. `technology` must outlive every use of the
    /// result: the wire struct only points at it.
    fn new(fields: &BatteryStatusFields, technology: Option<&CString>) -> Self {
        let (has_present, present) = slot(fields.present.map(c_int::from));
        let (has_percentage, percentage) = slot(fields.percentage);
        let (has_status, status) = slot(fields.status);
        let (has_health, health) = slot(fields.health);
        let (has_voltage_mv, voltage_mv) = slot(fields.voltage_mv);
        let (has_current_now_ua, current_now_ua) = slot(fields.current_now_ua);
        let (has_current_avg_ua, current_avg_ua) = slot(fields.current_avg_ua);
        let (has_charge_counter_uah, charge_counter_uah) = slot(fields.charge_counter_uah);
        let (has_power_now_uw, power_now_uw) = slot(fields.power_now_uw);
        let (has_temperature_c, temperature_c) = slot(fields.temperature_c);
        let (has_plugged, plugged) = slot(fields.plugged);
        BatteryWire {
            has_present,
            present,
            has_percentage,
            percentage,
            has_status,
            status,
            has_health,
            health,
            has_voltage_mv,
            voltage_mv,
            has_current_now_ua,
            current_now_ua,
            has_current_avg_ua,
            current_avg_ua,
            has_charge_counter_uah,
            charge_counter_uah,
            has_power_now_uw,
            power_now_uw,
            has_technology: c_int::from(technology.is_some()),
            technology: technology.map_or(std::ptr::null(), |t| t.as_ptr()),
            has_temperature_c,
            temperature_c,
            has_plugged,
            plugged,
        }
    }
}

/// `NativeGLInterface.reportBatteryStatus(BatteryStatus)`.
pub fn report_battery_status(native: *mut c_void, fields: &BatteryStatusFields) -> Result<(), String> {
    let technology = fields.technology.as_deref().map(c_text).transpose()?;
    let wire = BatteryWire::new(fields, technology.as_ref());
    // SAFETY: `wire` and the string it points at outlive the call.
    status(|err, len| unsafe { ffi::intoxicated_report_battery_status(native, &wire, err, len) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::call::field_offset;

    macro_rules! offset {
        ($field:ident) => {
            field_offset(|w: *const BatteryWire| unsafe { std::ptr::addr_of!((*w).$field) })
        };
    }

    #[test]
    fn the_wire_struct_matches_the_c_layout() {
        assert_eq!(std::mem::size_of::<BatteryWire>(), 104);
        assert_eq!(std::mem::align_of::<BatteryWire>(), 8);
        let ints = [
            offset!(has_present),
            offset!(present),
            offset!(has_percentage),
            offset!(percentage),
            offset!(has_status),
            offset!(status),
            offset!(has_health),
            offset!(health),
            offset!(has_voltage_mv),
            offset!(voltage_mv),
            offset!(has_current_now_ua),
            offset!(current_now_ua),
            offset!(has_current_avg_ua),
            offset!(current_avg_ua),
            offset!(has_charge_counter_uah),
            offset!(charge_counter_uah),
            offset!(has_power_now_uw),
            offset!(power_now_uw),
            offset!(has_technology),
        ];
        for (k, at) in ints.into_iter().enumerate() {
            assert_eq!(at, 4 * k, "int slot {k}");
        }
        assert_eq!(offset!(technology), 80);
        assert_eq!(offset!(has_temperature_c), 88);
        assert_eq!(offset!(temperature_c), 92);
        assert_eq!(offset!(has_plugged), 96);
        assert_eq!(offset!(plugged), 100);
    }

    #[test]
    fn absent_fields_cross_as_absent_and_present_ones_as_themselves() {
        let empty = BatteryWire::new(&BatteryStatusFields::default(), None);
        assert_eq!((empty.has_present, empty.present), (0, 0));
        assert_eq!((empty.has_temperature_c, empty.temperature_c), (0, 0.0));
        assert_eq!(empty.has_technology, 0);
        assert!(empty.technology.is_null());

        let technology = CString::new("Li-ion").unwrap();
        let fields = BatteryStatusFields {
            present: Some(true),
            percentage: Some(0),
            temperature_c: Some(31.5),
            plugged: Some(2),
            technology: Some("Li-ion".into()),
            ..Default::default()
        };
        let wire = BatteryWire::new(&fields, Some(&technology));
        assert_eq!((wire.has_present, wire.present), (1, 1));
        // A real zero is present, which is the distinction the flags exist for.
        assert_eq!((wire.has_percentage, wire.percentage), (1, 0));
        assert_eq!((wire.has_status, wire.status), (0, 0));
        assert_eq!((wire.has_temperature_c, wire.temperature_c), (1, 31.5));
        assert_eq!((wire.has_plugged, wire.plugged), (1, 2));
        assert_eq!((wire.has_technology, wire.technology), (1, technology.as_ptr()));
    }

    #[test]
    fn a_nul_in_the_technology_is_refused_before_the_call() {
        let fields = BatteryStatusFields { technology: Some("Li\0ion".into()), ..Default::default() };
        assert!(report_battery_status(std::ptr::null_mut(), &fields).unwrap_err().contains("nul byte"));
    }
}
