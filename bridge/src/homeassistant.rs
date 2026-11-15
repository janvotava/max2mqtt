//! Home Assistant MQTT discovery, in the device-based format (one retained message per
//! device at `<discovery prefix>/device/<id>/config`, Home Assistant 2024.11+).
//!
//! The entities read the bridge's own state topics, so nothing else is published for
//! Home Assistant. Thermostats become `climate` entities:
//! - HA mode `auto` is the MAX! week program, `heat` is manual at the comfort temperature,
//!   `off` is manual at 4.5 °C (the valve closes).
//! - Presets: `boost`, and `eco` / `comfort` (the device's temperatures, keeping the mode).
//!
//! Only devices with a room are announced, the same rule that keeps the neighbours'
//! devices out of heating.
//!
//! Payloads use Home Assistant's abbreviated keys and put what all components share
//! (availability, state topic) at the top: a thermostat's announcement is about 3 KB
//! instead of 4. Topics are written out in full: device-based discovery rejects a
//! top-level `~` base topic ("not a valid option at ['~']").
//!
//! They are written straight from structs to text, without a JSON tree in between: that
//! tree took several times the text's size, which matters on a board with under 200 KB of
//! heap.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use serde::Serialize;
use serde_json::Value;

use crate::address::Address;
use crate::config::DeviceConfig;
use crate::protocol::DeviceType;

pub struct Discovery<'a> {
    pub discovery_prefix: &'a str,
    /// The bridge's topic prefix.
    pub prefix: &'a str,
    /// Our radio address, which identifies the bridge.
    pub bridge: Address,
}

fn id(address: Address) -> String {
    format!("max2mqtt_{}", address.to_string().to_lowercase())
}

/// Template that renders nothing when `field` is missing, so Home Assistant keeps the
/// previous state instead of logging a template error.
fn field(field: &str) -> String {
    format!("{{{{ value_json.{field} | default('') }}}}")
}

fn on_off(field: &str) -> String {
    format!("{{{{ ('ON' if value_json.{field} else 'OFF') if value_json.{field} is defined }}}}")
}

/// Off at 4.5 °C even without a mode: after a restart, a wall thermostat's broadcasts carry
/// its temperatures long before a status frame says which mode it is in.
const MODE_STATE: &str = "{% if value_json.mode is defined and value_json.mode == 'auto' %}auto\
{% elif value_json.desired_temperature is defined and value_json.desired_temperature <= 4.5 %}off\
{% elif value_json.mode is defined %}heat{% endif %}";
const MODE_COMMAND: &str = "{% if value == 'auto' %}{\"mode\": \"auto\"}\
{% elif value == 'off' %}{\"mode\": \"manual\", \"temperature\": \"off\"}\
{% else %}{\"mode\": \"manual\", \"temperature\": \"comfort\"}{% endif %}";
const PRESET_STATE: &str =
    "{% if value_json.mode is defined %}{{ 'boost' if value_json.mode == 'boost' else 'none' }}{% endif %}";
const PRESET_COMMAND: &str = "{% if value == 'boost' %}{\"mode\": \"boost\"}\
{% elif value in ['eco', 'comfort'] %}{\"temperature\": \"{{ value }}\"}\
{% else %}{\"mode\": \"auto\"}{% endif %}";
/// `heating` only while the burner runs for this room, see `Bridge::device_state`.
const ACTION: &str = "{% if value_json.desired_temperature is defined and value_json.desired_temperature <= 4.5 %}off\
{% elif value_json.heating is defined %}{{ 'heating' if value_json.heating else 'idle' }}{% endif %}";

/// Component keys per device type; keys from an earlier type are removed explicitly.
fn components(t: DeviceType) -> &'static [&'static str] {
    match t {
        DeviceType::HeatingThermostat | DeviceType::HeatingThermostatPlus => {
            &["climate", "temperature", "valve", "battery", "rssi"]
        }
        // No valve sensor: a wall thermostat's valve is the room's most-open radiator, which
        // has its own. The climate entity still uses it for heating/idle.
        DeviceType::WallThermostat => &["climate", "temperature", "battery", "rssi"],
        DeviceType::ShutterContact => &["window", "battery", "rssi"],
        DeviceType::PushButton => &["battery", "rssi"],
        DeviceType::Cube => &[],
    }
}

fn platform(key: &str) -> &'static str {
    match key {
        "climate" => "climate",
        "battery" | "window" | "furnace" => "binary_sensor",
        "pairing" => "switch",
        "restart" => "button",
        _ => "sensor",
    }
}

fn model(t: DeviceType) -> &'static str {
    match t {
        DeviceType::Cube => "MAX! Cube",
        DeviceType::HeatingThermostat => "MAX! Radiator Thermostat",
        DeviceType::HeatingThermostatPlus => "MAX! Radiator Thermostat+",
        DeviceType::WallThermostat => "MAX! Wall Thermostat",
        DeviceType::ShutterContact => "MAX! Window Sensor",
        DeviceType::PushButton => "MAX! Eco Switch",
    }
}

/// An announcement: what all its components share, and the components.
#[derive(Serialize)]
struct Payload<'a> {
    stat_t: &'a str,
    avty: [Availability<'a>; 1],
    o: Origin,
    dev: Device<'a>,
    cmps: BTreeMap<&'static str, Component>,
}

#[derive(Serialize)]
struct Availability<'a> {
    t: &'a str,
    val_tpl: &'static str,
}

#[derive(Serialize)]
struct Origin {
    name: &'static str,
    sw: &'static str,
}

const ORIGIN: Origin = Origin { name: "max2mqtt", sw: env!("CARGO_PKG_VERSION") };

#[derive(Serialize)]
struct Device<'a> {
    ids: [String; 1],
    name: &'a str,
    mf: &'static str,
    mdl: &'static str,
    sn: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    via_device: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sa: Option<&'a str>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum Component {
    Entity(Box<Entity>),
    /// Removes a component Home Assistant knows from before.
    Removed {
        p: &'static str,
    },
}

/// One entity. Its state topic is the payload's shared `stat_t`.
#[derive(Serialize, Default)]
struct Entity {
    p: &'static str,
    uniq_id: String,
    /// `None` (null) names the entity after its device.
    name: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dev_cla: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    unit_of_meas: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stat_cla: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ent_cat: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ic: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    en: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    val_tpl: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cmd_t: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stat_on: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stat_off: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pl_on: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pl_off: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pl_prs: Option<&'static str>,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    climate: Option<Climate>,
}

/// The thermostat part of a `climate` entity.
#[derive(Serialize)]
struct Climate {
    curr_temp_t: String,
    curr_temp_tpl: String,
    temp_stat_t: String,
    temp_stat_tpl: String,
    temp_cmd_t: String,
    temp_cmd_tpl: &'static str,
    mode_stat_t: String,
    mode_stat_tpl: &'static str,
    mode_cmd_t: String,
    mode_cmd_tpl: &'static str,
    modes: [&'static str; 3],
    pr_mode_stat_t: String,
    pr_mode_val_tpl: &'static str,
    pr_mode_cmd_t: String,
    pr_mode_cmd_tpl: &'static str,
    pr_modes: [&'static str; 3],
    act_t: String,
    act_tpl: &'static str,
    min_temp: f32,
    max_temp: f32,
    temp_step: f32,
    precision: f32,
    temp_unit: &'static str,
}

impl Climate {
    /// For the device whose state is published on `state`.
    fn new(state: &str) -> Self {
        let set = format!("{state}/set");
        Climate {
            curr_temp_t: state.into(),
            curr_temp_tpl: field("measured_temperature"),
            temp_stat_t: state.into(),
            temp_stat_tpl: field("desired_temperature"),
            temp_cmd_t: set.clone(),
            temp_cmd_tpl: "{\"temperature\": {{ value }}}",
            mode_stat_t: state.into(),
            mode_stat_tpl: MODE_STATE,
            mode_cmd_t: set.clone(),
            mode_cmd_tpl: MODE_COMMAND,
            modes: ["auto", "heat", "off"],
            pr_mode_stat_t: state.into(),
            pr_mode_val_tpl: PRESET_STATE,
            pr_mode_cmd_t: set.clone(),
            pr_mode_cmd_tpl: PRESET_COMMAND,
            pr_modes: ["boost", "eco", "comfort"],
            act_t: state.into(),
            act_tpl: ACTION,
            min_temp: 4.5,
            max_temp: 30.5,
            temp_step: 0.5,
            precision: 0.1,
            temp_unit: "C",
        }
    }
}

impl Discovery<'_> {
    pub fn device_topic(&self, address: Address) -> String {
        format!("{}/device/{}/config", self.discovery_prefix, id(address))
    }

    pub fn status_topic(&self) -> String {
        format!("{}/status", self.discovery_prefix)
    }

    fn payload<'a>(&'a self, state: &'a str, dev: Device<'a>, cmps: BTreeMap<&'static str, Component>) -> String {
        let p = Payload {
            stat_t: state,
            avty: [Availability { t: self.prefix, val_tpl: "{{ value_json.availability }}" }],
            o: ORIGIN,
            dev,
            cmps,
        };
        serde_json::to_string(&p).unwrap_or_default()
    }

    /// The bridge itself: burner relay, heat demand, pairing switch, restart button.
    pub fn bridge(&self) -> (String, String) {
        let uid = id(self.bridge);
        let entity =
            |key: &str, p, name| Entity { p, uniq_id: format!("{uid}_{key}"), name: Some(name), ..Default::default() };
        let mut cmps = BTreeMap::new();
        cmps.insert(
            "furnace",
            Component::Entity(Box::new(Entity {
                dev_cla: Some("heat"),
                val_tpl: Some(on_off("furnace_running")),
                ..entity("furnace", "binary_sensor", "Furnace")
            })),
        );
        cmps.insert(
            "heat_demand",
            Component::Entity(Box::new(Entity {
                unit_of_meas: Some("%"),
                stat_cla: Some("measurement"),
                ic: Some("mdi:fire"),
                val_tpl: Some(field("heat_demand")),
                ..entity("heat_demand", "sensor", "Heat demand")
            })),
        );
        cmps.insert(
            "pairing",
            Component::Entity(Box::new(Entity {
                ent_cat: Some("config"),
                ic: Some("mdi:link-variant"),
                val_tpl: Some(on_off("pairing_enabled")),
                stat_on: Some("ON"),
                stat_off: Some("OFF"),
                cmd_t: Some(format!("{}/set", self.prefix)),
                pl_on: Some("{\"pair\": true}"),
                pl_off: Some("{\"pair\": false}"),
                ..entity("pairing", "switch", "Pairing")
            })),
        );
        cmps.insert(
            "restart",
            Component::Entity(Box::new(Entity {
                dev_cla: Some("restart"),
                ent_cat: Some("config"),
                cmd_t: Some(format!("{}/reset", self.prefix)),
                pl_prs: Some(""),
                ..entity("restart", "button", "Restart")
            })),
        );
        let dev = Device {
            ids: [uid.clone()],
            name: "max2mqtt",
            mf: "max2mqtt",
            mdl: "MAX! to MQTT bridge",
            sn: self.bridge.to_string(),
            via_device: None,
            sa: None,
        };
        (self.device_topic(self.bridge), self.payload(self.prefix, dev, cmps))
    }

    /// One MAX! device. `previous` is the type announced before, whose components that
    /// this type does not have are removed.
    pub fn device(&self, d: &DeviceConfig, t: DeviceType, previous: Option<DeviceType>) -> (String, String) {
        let uid = id(d.address);
        let state = format!("{}/{}", self.prefix, d.name);
        let mut cmps = BTreeMap::new();
        for &key in components(t) {
            let base = Entity { p: platform(key), uniq_id: format!("{uid}_{key}"), ..Default::default() };
            let e = match key {
                "climate" => Entity { climate: Some(Climate::new(&state)), ..base },
                "temperature" => Entity {
                    name: Some("Temperature"),
                    dev_cla: Some("temperature"),
                    unit_of_meas: Some("°C"),
                    stat_cla: Some("measurement"),
                    val_tpl: Some(field("measured_temperature")),
                    ..base
                },
                "valve" => Entity {
                    name: Some("Valve"),
                    unit_of_meas: Some("%"),
                    stat_cla: Some("measurement"),
                    ic: Some("mdi:valve"),
                    val_tpl: Some(field("valve_position")),
                    ..base
                },
                "battery" => Entity {
                    name: Some("Battery"),
                    dev_cla: Some("battery"),
                    ent_cat: Some("diagnostic"),
                    val_tpl: Some(on_off("low_battery")),
                    ..base
                },
                "window" => {
                    Entity { name: Some("Window"), dev_cla: Some("window"), val_tpl: Some(on_off("open")), ..base }
                }
                "rssi" => Entity {
                    name: Some("Signal"),
                    dev_cla: Some("signal_strength"),
                    unit_of_meas: Some("dBm"),
                    stat_cla: Some("measurement"),
                    ent_cat: Some("diagnostic"),
                    en: Some(false),
                    val_tpl: Some(field("rssi")),
                    ..base
                },
                _ => continue,
            };
            cmps.insert(key, Component::Entity(Box::new(e)));
        }
        if let Some(prev) = previous {
            for &key in components(prev) {
                cmps.entry(key).or_insert(Component::Removed { p: platform(key) });
            }
        }
        // Announced by earlier versions.
        if t == DeviceType::WallThermostat {
            cmps.entry("valve").or_insert(Component::Removed { p: "sensor" });
        }
        let dev = Device {
            ids: [uid],
            name: &d.name,
            mf: "eQ-3",
            mdl: model(t),
            sn: d.address.to_string(),
            via_device: Some(id(self.bridge)),
            sa: d.room.as_deref(),
        };
        (self.device_topic(d.address), self.payload(&state, dev, cmps))
    }
}

/// Topics of all components, for tests and debugging.
pub fn component_keys(payload: &str) -> Vec<String> {
    serde_json::from_str::<Value>(payload)
        .ok()
        .and_then(|v| v.get("cmps").and_then(Value::as_object).map(|m| m.keys().cloned().collect()))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn disco() -> Discovery<'static> {
        Discovery { discovery_prefix: "homeassistant", prefix: "max", bridge: Address([0x12, 0x34, 0x56]) }
    }

    fn heater() -> DeviceConfig {
        DeviceConfig {
            address: Address([0x15, 0xA5, 0x2A]),
            name: "living-room/heater".into(),
            room: Some("living-room".into()),
            ..Default::default()
        }
    }

    #[test]
    fn heater_climate_reads_device_topic() {
        let (topic, payload) = disco().device(&heater(), DeviceType::HeatingThermostat, None);
        assert_eq!(topic, "homeassistant/device/max2mqtt_15a52a/config");
        let v: Value = serde_json::from_str(&payload).unwrap();
        let c = &v["cmps"]["climate"];
        assert_eq!(c["p"], "climate");
        // Home Assistant rejects a top-level `~` in device discovery.
        assert!(!payload.contains('~'), "{payload}");
        assert_eq!(v["stat_t"], "max/living-room/heater");
        assert_eq!(c["curr_temp_t"], "max/living-room/heater");
        assert_eq!(c["temp_cmd_t"], "max/living-room/heater/set");
        assert_eq!(v["avty"][0]["t"], "max");
        assert_eq!(v["dev"]["via_device"], "max2mqtt_123456");
        assert_eq!(v["dev"]["sa"], "living-room");
        // Was 4 KB with full key names.
        assert!(payload.len() < 3500, "{} bytes", payload.len());
    }

    #[test]
    fn type_change_removes_old_components() {
        let (_, payload) = disco().device(&heater(), DeviceType::ShutterContact, Some(DeviceType::HeatingThermostat));
        let v: Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(v["cmps"]["window"]["p"], "binary_sensor");
        assert_eq!(v["cmps"]["climate"], json!({"p": "climate"}));
        assert_eq!(v["cmps"]["valve"], json!({"p": "sensor"}));
        assert!(v["cmps"]["battery"]["val_tpl"].is_string());
    }

    #[test]
    fn templates_are_valid_json_commands() {
        // The command templates must produce JSON the bridge accepts once Jinja fills in a value.
        assert!(MODE_COMMAND.contains("{\"mode\": \"auto\"}"));
        assert!(PRESET_COMMAND.contains("{\"mode\": \"boost\"}"));
        assert_eq!(field("rssi"), "{{ value_json.rssi | default('') }}");
    }

    #[test]
    fn bridge_device() {
        let (topic, payload) = disco().bridge();
        assert_eq!(topic, "homeassistant/device/max2mqtt_123456/config");
        let keys = component_keys(&payload);
        assert!(keys.iter().any(|k| k == "furnace") && keys.iter().any(|k| k == "pairing"), "{keys:?}");
        assert!(!payload.contains('~'), "{payload}");
        let v: Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(v["cmps"]["pairing"]["cmd_t"], "max/set");
        assert_eq!(v["cmps"]["restart"]["cmd_t"], "max/reset");
    }
}
