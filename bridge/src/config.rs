//! Persisted configuration. Stored as one JSON document in flash.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;

use serde::{Deserialize, Serialize};

use crate::address::Address;
use crate::clock::{TimeZone, Weekday};
use crate::protocol::DeviceType;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Our own radio address, as the "cube" the devices are paired to.
    pub address: Address,
    /// Create devices for every MAX! sender we hear. They are listed and published, but
    /// only devices with a room take part in heating, time sync and re-pairing.
    pub autocreate: bool,
    /// Keep to the 1 % duty cycle of the 868 MHz band (36 s of airtime per hour), like culfw.
    /// The old firmware had this switched off.
    pub duty_cycle_limit: bool,
    pub timezone: TimeZone,
    pub heating: HeatingConfig,
    pub devices: Vec<DeviceConfig>,
    /// Set once devices record what they have not confirmed ([`DeviceConfig::pending`]).
    /// A config from before that has everything resent once.
    pub tracking: bool,
}

impl Config {
    pub fn from_json(json: &str) -> Result<Self, String> {
        serde_json::from_str(json).map_err(|e| alloc::format!("{e}"))
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            address: Address([0x12, 0x34, 0x56]),
            autocreate: true,
            duty_cycle_limit: true,
            timezone: TimeZone::default(),
            heating: HeatingConfig::default(),
            devices: Vec::new(),
            tracking: false,
        }
    }
}

/// Burner control: the most-open valve drives a time-proportional (TPI) cycle, see
/// [`crate::heating`]. Settings of the earlier PID (`kp`, `ki`, ...) are ignored.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HeatingConfig {
    pub cycle_minutes: f32,
    pub min_on_minutes: f32,
    pub min_off_minutes: f32,
    /// Valves open less than this (%) do not call for heat.
    pub min_valve_position: u8,
    /// Readings older than this are ignored.
    pub stale_minutes: u32,
}

impl HeatingConfig {
    /// Values the burner logic can work with (the same limits as the web form). Without limits, a huge minimum on time keeps
    /// the burner going after every valve has closed, and a zero cycle means always on.
    pub fn validate(&self) -> Result<(), String> {
        let check = |ok: bool, what: &str| if ok { Ok(()) } else { Err(alloc::format!("heating: {what}")) };
        check((5.0..=60.0).contains(&self.cycle_minutes), "cycle 5-60 minutes")?;
        check((0.0..=30.0).contains(&self.min_on_minutes), "minimum on time 0-30 minutes")?;
        check((0.0..=30.0).contains(&self.min_off_minutes), "minimum off time 0-30 minutes")?;
        check(self.min_valve_position <= 100, "valve threshold 0-100 %")?;
        check((3..=120).contains(&self.stale_minutes), "readings expire after 3-120 minutes")
    }
}

impl Default for HeatingConfig {
    fn default() -> Self {
        HeatingConfig {
            // 20 rather than 15 minutes: about 13 % fewer burner starts at the same comfort,
            // in a simulation of four rooms; 10 minutes was worse on both counts.
            cycle_minutes: 20.0,
            min_on_minutes: 3.0,
            min_off_minutes: 5.0,
            min_valve_position: 30,
            stale_minutes: 10,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DeviceConfig {
    pub address: Address,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub room: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub device_type: Option<DeviceType>,
    pub group: u8,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub associations: Vec<Address>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_actual_temperature: Option<bool>,
    pub temperatures: Temperatures,
    pub valve: ValveConfig,
    /// Start time `"HH:MM"` to °C, per day. See [`crate::schedule`].
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub schedule: BTreeMap<Weekday, BTreeMap<String, f32>>,
    /// Settings sent to the device that it has not acknowledged yet. Stored, so they are
    /// sent again after a restart, and when the device is next heard.
    #[serde(skip_serializing_if = "BTreeSet::is_empty")]
    pub pending: BTreeSet<Pending>,
}

/// One setting a device has to confirm.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pending {
    Group,
    Link(Address),
    /// From configs before [`Pending::Unlinked`]: resent only while the partner is known.
    Unlink(Address),
    /// Remove the link to a partner, with its type: still sent after the partner was
    /// forgotten, which is when it matters most (a device that keeps trying to reach a
    /// partner that is gone reports a radio error).
    Unlinked(Address, crate::protocol::DeviceType),
    Temperatures,
    Valve,
    Display,
    Program(Weekday),
}

impl Default for DeviceConfig {
    fn default() -> Self {
        DeviceConfig {
            address: Address::BROADCAST,
            name: String::new(),
            room: None,
            device_type: None,
            group: 0,
            associations: Vec::new(),
            display_actual_temperature: None,
            temperatures: Temperatures::default(),
            valve: ValveConfig::default(),
            schedule: BTreeMap::new(),
            pending: BTreeSet::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Temperatures {
    pub comfort: f32,
    pub eco: f32,
    pub max: f32,
    pub min: f32,
    pub window_open: f32,
    pub window_open_minutes: u8,
    pub measurement_offset: f32,
}

impl Default for Temperatures {
    fn default() -> Self {
        Temperatures {
            comfort: 21.0,
            eco: 17.0,
            max: 30.5,
            min: 4.5,
            window_open: 4.5,
            window_open_minutes: 15,
            measurement_offset: 0.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ValveConfig {
    pub decalc_day: Weekday,
    pub decalc_hour: u8,
    /// Minutes (0, 5, ..., 30 or 60). The old firmware mixed minutes and protocol codes here.
    pub boost_minutes: u8,
    pub boost_valve_position: u8,
    pub max_valve_setting: u8,
    pub valve_offset: u8,
}

impl Default for ValveConfig {
    fn default() -> Self {
        ValveConfig {
            decalc_day: Weekday::Saturday,
            decalc_hour: 12,
            boost_minutes: 30,
            boost_valve_position: 100,
            max_valve_setting: 100,
            valve_offset: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_json_gives_defaults() {
        let c: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(c, Config::default());
    }

    #[test]
    fn roundtrip_with_device() {
        let mut c = Config::default();
        let mut d =
            DeviceConfig { address: Address([1, 2, 3]), name: "living-room/heater".into(), ..Default::default() };
        d.room = Some("living-room".into());
        d.device_type = Some(DeviceType::HeatingThermostat);
        d.schedule.insert(Weekday::Monday, [("06:00".into(), 21.5)].into_iter().collect());
        c.devices.push(d);
        let json = serde_json::to_string(&c).unwrap();
        assert!(json.contains("\"monday\":{\"06:00\":21.5}"), "{json}");
        assert!(json.contains("\"type\":\"heater\""), "{json}");
        assert_eq!(serde_json::from_str::<Config>(&json).unwrap(), c);
    }
}
