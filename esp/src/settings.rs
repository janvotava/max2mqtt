//! Settings for the first boot, before anything is configured in the web UI. They come from
//! `.cargo/config.toml` and the optional `secrets.toml` (see `secrets.example.toml`), and
//! all of them are optional: without Wi-Fi settings the board opens its setup hotspot.
//! Once settings are saved in the web UI, the stored ones win.

use alloc::string::String;

use max2mqtt_bridge::Settings;
use max2mqtt_bridge::settings::{MqttSettings, WifiSettings};

fn env(v: Option<&'static str>) -> String {
    v.unwrap_or_default().into()
}

pub fn defaults() -> Settings {
    let d = Settings::default();
    let md = MqttSettings::default();
    Settings {
        hostname: option_env!("MAX_HOSTNAME").filter(|s| !s.is_empty()).map(String::from).unwrap_or(d.hostname),
        // Empty: generated on first boot, see main.
        password: env(option_env!("MAX_PASSWORD")),
        wifi: WifiSettings { ssid: env(option_env!("MAX_WIFI_SSID")), password: env(option_env!("MAX_WIFI_PASSWORD")) },
        mqtt: MqttSettings {
            host: env(option_env!("MAX_MQTT_HOST")),
            port: option_env!("MAX_MQTT_PORT").and_then(|p| p.parse().ok()).unwrap_or(md.port),
            user: env(option_env!("MAX_MQTT_USER")),
            password: env(option_env!("MAX_MQTT_PASSWORD")),
            prefix: option_env!("MAX_MQTT_PREFIX").filter(|s| !s.is_empty()).map(String::from).unwrap_or(md.prefix),
            homeassistant: option_env!("MAX_HOMEASSISTANT")
                .map(|v| v != "false" && v != "0")
                .unwrap_or(md.homeassistant),
            discovery_prefix: md.discovery_prefix,
        },
        ntp_server: option_env!("MAX_NTP_SERVER").filter(|s| !s.is_empty()).map(String::from).unwrap_or(d.ntp_server),
        cc1101_freq_offset: option_env!("MAX_CC1101_FREQ_OFFSET").and_then(|v| v.parse().ok()).unwrap_or(0),
    }
}

// A typo in the frequency offset would make the radio deaf without any error, so check it
// while compiling.
const _: () = if let Some(v) = option_env!("MAX_CC1101_FREQ_OFFSET") {
    check_i8(v);
};

const fn check_i8(s: &str) {
    let b = s.as_bytes();
    let start = matches!(b.first(), Some(b'-' | b'+')) as usize;
    assert!(b.len() > start, "MAX_CC1101_FREQ_OFFSET is empty");
    let mut i = start;
    let mut v: i32 = 0;
    while i < b.len() {
        assert!(b[i].is_ascii_digit(), "MAX_CC1101_FREQ_OFFSET must be an integer");
        v = v * 10 + (b[i] - b'0') as i32;
        assert!(v <= 128, "MAX_CC1101_FREQ_OFFSET must be -128..127");
        i += 1;
    }
    assert!(!(v == 128 && b[0] != b'-'), "MAX_CC1101_FREQ_OFFSET must be -128..127");
}
