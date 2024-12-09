//! Export and restore: the board's settings and the bridge config in one JSON file.
//!
//! A backup holds everything needed to set up a replacement board, or this one after a
//! factory reset: Wi-Fi, MQTT, the radio address the devices are paired to, and every
//! device with its room, temperatures and week program. It contains the Wi-Fi and MQTT
//! passwords in plain text. The device password is not in it: a restore keeps the board's.
//!
//! Restoring marks every device setting as unconfirmed, so the bridge sends it all again:
//! the devices may have been changed since the backup was taken.

use alloc::string::String;

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::settings::Settings;

pub const FORMAT: &str = "max2mqtt-backup";
pub const VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
pub struct Backup {
    pub format: String,
    pub version: u32,
    /// The firmware that wrote it, for information.
    #[serde(default)]
    pub firmware: String,
    pub settings: Settings,
    pub config: Config,
}

impl Backup {
    pub fn new(mut settings: Settings, config: Config, firmware: &str) -> Self {
        settings.password.clear();
        Backup { format: FORMAT.into(), version: VERSION, firmware: firmware.into(), settings, config }
    }

    /// Compact: a restore has to fit the board's request size limit.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// Parse and check a backup for restoring, keeping the board's device `password`.
    pub fn restore(json: &[u8], password: &str) -> Result<Self, String> {
        // Only the format field: the rest is skipped, not built into a JSON tree.
        #[derive(Deserialize)]
        struct Probe {
            #[serde(default)]
            format: String,
        }
        let probe: Probe = serde_json::from_slice(json).map_err(|e| alloc::format!("not a JSON file: {e}"))?;
        if probe.format != FORMAT {
            return Err("not a max2mqtt backup".into());
        }
        let mut backup: Backup = serde_json::from_slice(json).map_err(|e| alloc::format!("invalid backup: {e}"))?;
        if backup.version > VERSION {
            return Err(alloc::format!(
                "backup format {} is newer than this firmware understands ({VERSION}): update the firmware first",
                backup.version
            ));
        }
        backup.settings.password = password.into();
        backup.settings.validate().map_err(|e| alloc::format!("settings in the backup: {e}"))?;
        backup.config.heating.validate()?;
        let mut seen = alloc::collections::BTreeSet::new();
        for d in &backup.config.devices {
            if !seen.insert(d.address) {
                return Err(alloc::format!("device {} is in the backup twice", d.address));
            }
        }
        backup.config.tracking = false;
        for d in &mut backup.config.devices {
            d.pending.clear();
        }
        Ok(backup)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::address::Address;
    use crate::config::DeviceConfig;

    fn sample() -> Backup {
        let mut settings = Settings { password: "old-device-password".into(), ..Default::default() };
        settings.wifi.ssid = "home".into();
        settings.wifi.password = "wifi-password".into();
        let mut config = Config { address: Address([0x0E, 0xE3, 0xDE]), tracking: true, ..Default::default() };
        config.devices.push(DeviceConfig {
            address: Address([0x15, 0xA5, 0x2A]),
            name: "bedroom/heater".into(),
            room: Some("bedroom".into()),
            ..Default::default()
        });
        Backup::new(settings, config, "0.2.0")
    }

    #[test]
    fn round_trip_keeps_everything_and_resends() {
        let json = sample().to_json();
        assert!(json.contains("wifi-password"), "a restored board has to get onto the network");
        assert!(!json.contains("old-device-password"), "the device password stays out");
        let b = Backup::restore(json.as_bytes(), "current-password").unwrap();
        assert_eq!(b.settings.password, "current-password");
        assert_eq!(b.settings.wifi.password, "wifi-password");
        assert_eq!(b.config.address, Address([0x0E, 0xE3, 0xDE]));
        assert_eq!(b.config.devices[0].room.as_deref(), Some("bedroom"));
        assert!(!b.config.tracking, "restored settings are sent to the devices again");
    }

    #[test]
    fn rejects_other_files() {
        let pw = "current-password";
        assert!(Backup::restore(b"{}", pw).unwrap_err().contains("not a max2mqtt backup"));
        assert!(Backup::restore(b"nope", pw).unwrap_err().contains("not a JSON file"));
        let mut b = sample();
        b.version = VERSION + 1;
        assert!(Backup::restore(b.to_json().as_bytes(), pw).unwrap_err().contains("newer"));
        let mut b = sample();
        b.settings.hostname = "not a hostname".into();
        assert!(Backup::restore(b.to_json().as_bytes(), pw).unwrap_err().contains("hostname"));
        let mut b = sample();
        b.config.heating.min_on_minutes = 1e6;
        assert!(Backup::restore(b.to_json().as_bytes(), pw).unwrap_err().contains("minimum on"));
        let mut b = sample();
        let d = b.config.devices[0].clone();
        b.config.devices.push(d);
        assert!(Backup::restore(b.to_json().as_bytes(), pw).unwrap_err().contains("twice"));
    }
}
