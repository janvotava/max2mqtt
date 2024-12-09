//! Device settings: network, MQTT, radio. Edited in the web UI, stored in flash next to the
//! bridge [`Config`](crate::Config). Changes take effect after a restart.
//!
//! The web API never returns passwords. It returns `"password_set": true` instead, and a
//! patch without a password field keeps the stored one.

use alloc::format;
use alloc::string::{String, ToString};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// DHCP hostname, also the hotspot name prefix.
    pub hostname: String,
    /// Web UI password, and the WPA2 password of the setup hotspot (8-63 characters).
    pub password: String,
    pub wifi: WifiSettings,
    pub mqtt: MqttSettings,
    pub ntp_server: String,
    /// CC1101 crystal correction in steps of 1.587 kHz. See docs/cc1101-shield.md.
    pub cc1101_freq_offset: i8,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WifiSettings {
    /// Empty: no network configured, the hotspot stays on.
    pub ssid: String,
    pub password: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MqttSettings {
    /// Empty: MQTT off.
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    /// First topic level of everything the bridge publishes and listens to.
    pub prefix: String,
    /// Announce devices to Home Assistant (MQTT discovery).
    pub homeassistant: bool,
    pub discovery_prefix: String,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            hostname: "max".into(),
            password: String::new(),
            wifi: WifiSettings::default(),
            mqtt: MqttSettings::default(),
            ntp_server: "pool.ntp.org".into(),
            cc1101_freq_offset: 0,
        }
    }
}

impl Default for MqttSettings {
    fn default() -> Self {
        MqttSettings {
            host: String::new(),
            port: 1883,
            user: String::new(),
            password: String::new(),
            prefix: "max".into(),
            homeassistant: true,
            discovery_prefix: "homeassistant".into(),
        }
    }
}

/// Paths of the secret fields, as (section, key). `None` is the top level.
const SECRETS: [(Option<&str>, &str); 3] = [(None, "password"), (Some("wifi"), "password"), (Some("mqtt"), "password")];

fn section<'a>(v: &'a mut Value, name: Option<&str>) -> Option<&'a mut Map<String, Value>> {
    match name {
        None => v.as_object_mut(),
        Some(n) => v.get_mut(n)?.as_object_mut(),
    }
}

/// A valid MQTT topic (prefix) without wildcards or empty levels.
pub fn valid_topic(t: &str) -> bool {
    !t.is_empty() && !t.contains(['+', '#', '\0']) && !t.starts_with('/') && !t.ends_with('/') && !t.contains("//")
}

impl Settings {
    pub fn from_json(json: &str) -> Result<Self, String> {
        serde_json::from_str(json).map_err(|e| e.to_string())
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// For the web API: secrets replaced by `<name>_set` flags.
    pub fn redacted(&self) -> Value {
        let mut v = serde_json::to_value(self).unwrap_or_default();
        for (sec, key) in SECRETS {
            if let Some(obj) = section(&mut v, sec)
                && let Some(Value::String(s)) = obj.remove(key)
            {
                obj.insert(format!("{key}_set"), Value::Bool(!s.is_empty()));
            }
        }
        v
    }

    /// Apply a (partial) JSON object from the web UI. Missing fields, and missing or null
    /// passwords, keep their current value. Returns the validated result.
    pub fn patched(&self, patch: &Value) -> Result<Settings, String> {
        let Some(patch) = patch.as_object() else {
            return Err("settings must be a JSON object".into());
        };
        let mut base = serde_json::to_value(self).map_err(|e| e.to_string())?;
        merge(&mut base, &Value::Object(patch.clone()));
        let settings: Settings = serde_json::from_value(base).map_err(|e| e.to_string())?;
        settings.validate()?;
        Ok(settings)
    }

    pub fn validate(&self) -> Result<(), String> {
        let h = &self.hostname;
        if h.is_empty()
            || h.len() > 32
            || !h.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || h.starts_with('-')
            || h.ends_with('-')
        {
            return Err("hostname: 1-32 letters, digits and dashes".into());
        }
        if !(8..=63).contains(&self.password.len()) {
            return Err("password: 8-63 characters".into());
        }
        if self.wifi.ssid.len() > 32 {
            return Err("wifi.ssid: at most 32 bytes".into());
        }
        let wp = self.wifi.password.len();
        if wp != 0 && !(8..=64).contains(&wp) {
            return Err("wifi.password: empty (open network) or 8-64 characters".into());
        }
        if self.mqtt.host.len() > 253 || self.mqtt.host.contains(char::is_whitespace) {
            return Err("mqtt.host: not a host name".into());
        }
        if self.mqtt.port == 0 {
            return Err("mqtt.port: 1-65535".into());
        }
        if !valid_topic(&self.mqtt.prefix) {
            return Err("mqtt.prefix: a topic without +, #, or empty levels".into());
        }
        if !valid_topic(&self.mqtt.discovery_prefix) {
            return Err("mqtt.discovery_prefix: a topic without +, #, or empty levels".into());
        }
        if self.ntp_server.is_empty() || self.ntp_server.contains(char::is_whitespace) {
            return Err("ntp_server: a host name".into());
        }
        Ok(())
    }
}

/// JSON merge patch (RFC 7396), except that null does not delete: it keeps the value.
fn merge(base: &mut Value, patch: &Value) {
    match (base, patch) {
        (Value::Object(b), Value::Object(p)) => {
            for (k, v) in p {
                match b.get_mut(k) {
                    Some(existing) => merge(existing, v),
                    None if !v.is_null() => {
                        b.insert(k.clone(), v.clone());
                    }
                    None => {}
                }
            }
        }
        (_, Value::Null) => {}
        (b, p) => *b = p.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn valid() -> Settings {
        Settings { password: "correct horse".into(), ..Settings::default() }
    }

    #[test]
    fn redacted_hides_passwords() {
        let mut s = valid();
        s.mqtt.password = "secret".into();
        let v = s.redacted();
        let text = v.to_string();
        assert!(!text.contains("secret") && !text.contains("correct horse"), "{text}");
        assert_eq!(v["password_set"], true);
        assert_eq!(v["wifi"]["password_set"], false);
        assert_eq!(v["mqtt"]["password_set"], true);
    }

    #[test]
    fn patch_keeps_unmentioned_and_null_fields() {
        let mut s = valid();
        s.wifi = WifiSettings { ssid: "home".into(), password: "wifi-password".into() };
        let p = s.patched(&json!({"mqtt": {"host": "mqtt.lan", "prefix": "max-test"}, "wifi": {"password": null}}));
        let p = p.unwrap();
        assert_eq!(p.mqtt.host, "mqtt.lan");
        assert_eq!(p.mqtt.prefix, "max-test");
        assert_eq!(p.mqtt.port, 1883);
        assert_eq!(p.wifi.password, "wifi-password");
        assert_eq!(p.password, "correct horse");
    }

    #[test]
    fn patch_validates() {
        let s = valid();
        assert!(s.patched(&json!({"password": "short"})).is_err());
        assert!(s.patched(&json!({"hostname": "bad name"})).is_err());
        assert!(s.patched(&json!({"mqtt": {"prefix": "max/#"}})).is_err());
        assert!(s.patched(&json!({"mqtt": {"port": 0}})).is_err());
        assert!(s.patched(&json!({"cc1101_freq_offset": 200})).is_err());
        assert!(s.patched(&json!([])).is_err());
        assert_eq!(s.patched(&json!({"cc1101_freq_offset": -12})).unwrap().cc1101_freq_offset, -12);
    }

    #[test]
    fn missing_fields_get_defaults() {
        let s = Settings::from_json(r#"{"hostname":"boiler"}"#).unwrap();
        assert_eq!(s.hostname, "boiler");
        assert_eq!(s.mqtt.discovery_prefix, "homeassistant");
    }
}
