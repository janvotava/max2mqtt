//! The MAX! radio protocol: frame layout, decoding what devices send, encoding commands.
//!
//! Byte offsets and encodings were checked against FHEM's `10_MAX.pm` / `14_CUL_MAX.pm`, the
//! most widely used open implementation. Where the old C++ firmware disagreed with FHEM, FHEM
//! wins and the test names say so.
//!
//! Frame layout (what the CC1101 puts in its RX FIFO, with `APPEND_STATUS` enabled):
//!
//! ```text
//! [len] [msgcnt] [flags] [cmd] [src x3] [dst x3] [group] [payload ...] [rssi] [lqi|crc_ok]
//!   0      1        2      3     4..7     7..10    10       11..          len+1   len+2
//! ```
//! `len` counts the bytes after itself, so a frame without payload has `len = 10`.

use alloc::string::String;
use alloc::vec::Vec;

use serde::{Deserialize, Serialize};

use crate::address::Address;
use crate::clock::{LocalTime, Weekday};

pub mod command {
    pub const PAIR_PING: u8 = 0x00;
    pub const PAIR_PONG: u8 = 0x01;
    pub const ACK: u8 = 0x02;
    pub const TIME_INFORMATION: u8 = 0x03;
    pub const CONFIG_WEEK_PROFILE: u8 = 0x10;
    pub const CONFIG_TEMPERATURES: u8 = 0x11;
    pub const CONFIG_VALVE: u8 = 0x12;
    pub const ADD_LINK_PARTNER: u8 = 0x20;
    pub const REMOVE_LINK_PARTNER: u8 = 0x21;
    pub const SET_GROUP_ID: u8 = 0x22;
    pub const REMOVE_GROUP_ID: u8 = 0x23;
    pub const SHUTTER_CONTACT_STATE: u8 = 0x30;
    pub const SET_TEMPERATURE: u8 = 0x40;
    pub const WALL_THERMOSTAT_CONTROL: u8 = 0x42;
    pub const SET_COMFORT_TEMPERATURE: u8 = 0x43;
    pub const SET_ECO_TEMPERATURE: u8 = 0x44;
    pub const PUSH_BUTTON_STATE: u8 = 0x50;
    pub const THERMOSTAT_STATE: u8 = 0x60;
    pub const WALL_THERMOSTAT_STATE: u8 = 0x70;
    pub const SET_DISPLAY_ACTUAL_TEMPERATURE: u8 = 0x82;
    pub const RESET: u8 = 0xF0;
    pub const WAKE_UP: u8 = 0xF1;
}

/// Message flag set when the message is addressed to a group.
pub const FLAG_GROUP: u8 = 0x04;

/// Bytes in a frame before the payload, including the length byte.
pub const HEADER_LEN: usize = 11;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceType {
    #[serde(rename = "cube")]
    Cube,
    #[serde(rename = "heater")]
    HeatingThermostat,
    #[serde(rename = "heater_plus")]
    HeatingThermostatPlus,
    #[serde(rename = "thermostat")]
    WallThermostat,
    #[serde(rename = "shutter_contact")]
    ShutterContact,
    #[serde(rename = "push_button")]
    PushButton,
}

impl DeviceType {
    pub fn from_raw(raw: u8) -> Option<Self> {
        Some(match raw {
            0 => Self::Cube,
            1 => Self::HeatingThermostat,
            2 => Self::HeatingThermostatPlus,
            3 => Self::WallThermostat,
            4 => Self::ShutterContact,
            5 => Self::PushButton,
            _ => return None,
        })
    }

    pub fn raw(self) -> u8 {
        match self {
            Self::Cube => 0,
            Self::HeatingThermostat => 1,
            Self::HeatingThermostatPlus => 2,
            Self::WallThermostat => 3,
            Self::ShutterContact => 4,
            Self::PushButton => 5,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cube => "cube",
            Self::HeatingThermostat => "heater",
            Self::HeatingThermostatPlus => "heater_plus",
            Self::WallThermostat => "thermostat",
            Self::ShutterContact => "shutter_contact",
            Self::PushButton => "push_button",
        }
    }

    /// Radiator valve (heating thermostat).
    pub fn is_heater(self) -> bool {
        matches!(self, Self::HeatingThermostat | Self::HeatingThermostatPlus)
    }

    /// Anything with a setpoint: radiator valves and wall thermostats.
    pub fn is_thermostat(self) -> bool {
        self.is_heater() || self == Self::WallThermostat
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Auto,
    Manual,
    Temporary,
    Boost,
}

impl Mode {
    pub fn from_bits(bits: u8) -> Self {
        match bits & 0x03 {
            0 => Self::Auto,
            1 => Self::Manual,
            2 => Self::Temporary,
            _ => Self::Boost,
        }
    }

    pub fn bits(self) -> u8 {
        match self {
            Self::Auto => 0,
            Self::Manual => 1,
            Self::Temporary => 2,
            Self::Boost => 3,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Manual => "manual",
            Self::Temporary => "temporary",
            Self::Boost => "boost",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.trim() {
            s if s.eq_ignore_ascii_case("auto") => Self::Auto,
            s if s.eq_ignore_ascii_case("manual") => Self::Manual,
            s if s.eq_ignore_ascii_case("temporary") => Self::Temporary,
            s if s.eq_ignore_ascii_case("boost") => Self::Boost,
            _ => return None,
        })
    }
}

/// The status byte shared by thermostat, wall thermostat and shutter contact messages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Status {
    pub mode: Mode,
    pub dst_auto: bool,
    pub panel_locked: bool,
    pub rf_error: bool,
    pub low_battery: bool,
}

impl Status {
    pub fn from_byte(b: u8) -> Self {
        Status {
            mode: Mode::from_bits(b),
            dst_auto: b & 0x08 != 0,
            panel_locked: b & 0x20 != 0,
            rf_error: b & 0x40 != 0,
            low_battery: b & 0x80 != 0,
        }
    }
}

/// A decoded radio frame without the CC1101 status bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub msgcnt: u8,
    pub flags: u8,
    pub command: u8,
    pub src: Address,
    pub dst: Address,
    pub group: u8,
    pub payload: Vec<u8>,
}

impl Frame {
    /// Bytes to write to the TX FIFO, starting with the length byte.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_LEN + self.payload.len());
        out.push((HEADER_LEN - 1 + self.payload.len()) as u8);
        out.push(self.msgcnt);
        out.push(self.flags);
        out.push(self.command);
        out.extend_from_slice(&self.src.0);
        out.extend_from_slice(&self.dst.0);
        out.push(self.group);
        out.extend_from_slice(&self.payload);
        out
    }
}

/// A frame as received, with the link quality the CC1101 appended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RxFrame {
    pub frame: Frame,
    pub rssi_dbm: i16,
    pub lqi: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    Empty,
    /// The length byte does not match the number of bytes read from the FIFO.
    LengthMismatch {
        declared: usize,
        actual: usize,
    },
    /// Shorter than a MAX! header.
    TooShort,
    /// The CC1101 `CRC_OK` bit is clear.
    Crc,
}

/// RSSI register value to dBm (CC1101 datasheet section 17.3, 868 MHz offset 74).
pub fn rssi_dbm(raw: u8) -> i16 {
    (raw as i8 as i16) / 2 - 74
}

/// Decode the bytes read from the CC1101 RX FIFO: length byte, frame, RSSI, LQI/CRC.
///
/// The old firmware called a length check "CRC OK" and never looked at the real CRC bit.
pub fn decode_rx(raw: &[u8]) -> Result<RxFrame, DecodeError> {
    let (&len, _) = raw.split_first().ok_or(DecodeError::Empty)?;
    let len = len as usize;
    if raw.len() != len + 3 {
        return Err(DecodeError::LengthMismatch { declared: len, actual: raw.len() });
    }
    let rssi = raw[len + 1];
    let lqi = raw[len + 2];
    if lqi & 0x80 == 0 {
        return Err(DecodeError::Crc);
    }
    if len < HEADER_LEN - 1 {
        return Err(DecodeError::TooShort);
    }
    let body = &raw[1..=len];
    let frame = Frame {
        msgcnt: body[0],
        flags: body[1],
        command: body[2],
        src: Address([body[3], body[4], body[5]]),
        dst: Address([body[6], body[7], body[8]]),
        group: body[9],
        payload: body[10..].to_vec(),
    };
    Ok(RxFrame { frame, rssi_dbm: rssi_dbm(rssi), lqi: lqi & 0x7F })
}

/// State broadcast by a radiator valve (`0x60`), also carried in its ACKs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ThermostatState {
    pub status: Status,
    pub valve_position: u8,
    pub desired: f32,
    /// Only present outside temporary mode (the bytes hold the "until" date then).
    pub measured: Option<f32>,
}

/// State of a wall thermostat (`0x70`), also carried in its ACKs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WallThermostatState {
    pub status: Status,
    pub display_actual_temperature: bool,
    pub desired: f32,
    pub measured: Option<f32>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    PairPing {
        firmware: u8,
        device_type: Option<DeviceType>,
        serial: String,
    },
    /// `ok == false` is a NACK ("invalid command/argument"). `state` is the sender's
    /// status in the format of its own state message, without the leading ack byte.
    Ack {
        ok: bool,
        state: Vec<u8>,
    },
    /// A device asking us for the time.
    TimeRequest,
    /// Someone (another cube) broadcasting the time; ignored.
    TimeInformation,
    ThermostatState(ThermostatState),
    WallThermostatState(WallThermostatState),
    WallThermostatControl {
        desired: f32,
        measured: f32,
    },
    SetTemperature {
        mode: Mode,
        desired: f32,
    },
    ShutterContactState {
        open: bool,
        status: Status,
    },
    PushButtonState {
        status: Status,
    },
    /// A command we do not handle.
    Unknown,
    /// A known command with a payload too short to parse.
    Malformed,
}

fn half_degrees_to_celsius(raw: u8) -> f32 {
    (raw & 0x7F) as f32 / 2.0
}

/// Payload of `0x60`, as in FHEM `MAX_Parse` "ThermostatState".
pub fn parse_thermostat_state(p: &[u8]) -> Option<ThermostatState> {
    let (&status, rest) = p.split_first()?;
    let status = Status::from_byte(status);
    let valve_position = *rest.first()?;
    let desired = half_degrees_to_celsius(*rest.get(1)?);
    let measured = match (rest.get(2), rest.get(3)) {
        (Some(&hi), Some(&lo)) if status.mode != Mode::Temporary => {
            let t = ((((hi & 0x01) as u16) << 8) | lo as u16) as f32 / 10.0;
            // FHEM: valves very rarely report values like 0.2 °C; treat < 1 °C as missing.
            (t >= 1.0).then_some(t)
        }
        _ => None,
    };
    Some(ThermostatState { status, valve_position, desired, measured })
}

/// Payload of `0x70`, as in FHEM `MAX_Parse` "WallThermostatState".
///
/// The wall thermostat's own reading is the 7th payload byte, with its 9th bit in the top
/// bit of the setpoint byte. The old firmware read bytes 4-5 instead, which is the
/// temperature measured by a paired radiator valve, or an "until" date in temporary mode.
pub fn parse_wall_thermostat_state(p: &[u8]) -> Option<WallThermostatState> {
    let status = Status::from_byte(*p.first()?);
    let display_actual_temperature = *p.get(1)? != 0;
    let raw_desired = *p.get(2)?;
    let measured = p.get(6).map(|&lo| ((((raw_desired & 0x80) as u16) << 1) | lo as u16) as f32 / 10.0);
    Some(WallThermostatState {
        status,
        display_actual_temperature,
        desired: half_degrees_to_celsius(raw_desired),
        measured,
    })
}

pub fn parse(frame: &Frame) -> Message {
    use command::*;
    let p = frame.payload.as_slice();
    let parsed = match frame.command {
        PAIR_PING => p.get(..3).map(|head| Message::PairPing {
            firmware: head[0],
            device_type: DeviceType::from_raw(head[1]),
            serial: p[3..].iter().filter(|b| b.is_ascii_graphic()).map(|&b| b as char).collect(),
        }),
        ACK => p.split_first().map(|(&code, rest)| Message::Ack { ok: code & 0x80 == 0, state: rest.to_vec() }),
        TIME_INFORMATION => Some(if p.is_empty() { Message::TimeRequest } else { Message::TimeInformation }),
        THERMOSTAT_STATE => parse_thermostat_state(p).map(Message::ThermostatState),
        WALL_THERMOSTAT_STATE => parse_wall_thermostat_state(p).map(Message::WallThermostatState),
        WALL_THERMOSTAT_CONTROL => match p {
            [raw_desired, lo, ..] => Some(Message::WallThermostatControl {
                desired: half_degrees_to_celsius(*raw_desired),
                measured: ((((raw_desired & 0x80) as u16) << 1) | *lo as u16) as f32 / 10.0,
            }),
            _ => None,
        },
        SET_TEMPERATURE => p
            .first()
            .map(|&b| Message::SetTemperature { mode: Mode::from_bits(b >> 6), desired: (b & 0x3F) as f32 / 2.0 }),
        SHUTTER_CONTACT_STATE => {
            p.first().map(|&b| Message::ShutterContactState { open: b & 0x03 != 0, status: Status::from_byte(b) })
        }
        PUSH_BUTTON_STATE => p.first().map(|&b| Message::PushButtonState { status: Status::from_byte(b) }),
        _ => return Message::Unknown,
    };
    parsed.unwrap_or(Message::Malformed)
}

/// Temperature in °C to the protocol's half-degree units.
pub fn half_degrees(celsius: f32) -> u8 {
    libm::roundf(celsius * 2.0).clamp(0.0, 255.0) as u8
}

/// Lowest and highest setpoints a MAX! thermostat accepts ("off" and "on").
pub const TEMPERATURE_MIN: f32 = 4.5;
pub const TEMPERATURE_MAX: f32 = 30.5;

/// End of a temporary mode, to the half hour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Until {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub half_past: bool,
}

impl Until {
    /// Parse `"YYYY-MM-DD HH:MM"` where the minutes are `00` or `30`.
    pub fn parse(s: &str) -> Option<Self> {
        let (date, time) = s.trim().split_once([' ', 'T'])?;
        let mut d = date.split('-');
        let year: u16 = d.next()?.parse().ok()?;
        let month: u8 = d.next()?.parse().ok()?;
        let day: u8 = d.next()?.parse().ok()?;
        let (h, m) = time.split_once(':')?;
        let hour: u8 = h.parse().ok()?;
        let minute: u8 = m.get(..2).unwrap_or(m).parse().ok()?;
        let valid = d.next().is_none()
            && (2000..2064).contains(&year)
            && (1..=12).contains(&month)
            && (1..=31).contains(&day)
            && hour < 24
            && (minute == 0 || minute == 30);
        valid.then_some(Until { year, month, day, hour, half_past: minute == 30 })
    }

    /// FHEM `MAX_Set` desiredTemperature ... until.
    pub fn encode(&self) -> [u8; 3] {
        let month = self.month as u32;
        let v = ((month & 0x0E) << 20)
            | ((self.day as u32) << 16)
            | ((month & 0x01) << 15)
            | (((self.year - 2000) as u32) << 8)
            | (self.hour as u32 * 2 + self.half_past as u32);
        [(v >> 16) as u8, (v >> 8) as u8, v as u8]
    }
}

/// Payloads of commands we send.
pub mod payload {
    use super::*;

    pub const PAIR_PONG: [u8; 1] = [0x00];
    pub const ACK_OK: [u8; 1] = [0x01];

    /// `0x40`. Temperature 0 in auto mode means "follow the week profile".
    pub fn set_temperature(mode: Mode, celsius: f32, until: Option<Until>) -> Vec<u8> {
        let mut p = alloc::vec![(half_degrees(celsius) & 0x3F) | (mode.bits() << 6)];
        if let Some(until) = until {
            p.extend_from_slice(&until.encode());
        }
        p
    }

    /// `0x11`.
    #[allow(clippy::too_many_arguments)]
    pub fn config_temperatures(
        comfort: f32,
        eco: f32,
        max: f32,
        min: f32,
        measurement_offset: f32,
        window_open: f32,
        window_open_minutes: u8,
    ) -> Vec<u8> {
        alloc::vec![
            half_degrees(comfort),
            half_degrees(eco),
            half_degrees(max),
            half_degrees(min),
            half_degrees(measurement_offset.clamp(-3.5, 3.5) + 3.5),
            half_degrees(window_open),
            window_open_minutes / 5,
        ]
    }

    /// Boost durations the protocol can express, in minutes, indexed by their 3-bit code.
    pub const BOOST_MINUTES: [u8; 8] = [0, 5, 10, 15, 20, 25, 30, 60];

    /// Round a boost duration down to one the device supports.
    pub fn boost_minutes_supported(minutes: u32) -> u8 {
        BOOST_MINUTES.iter().rev().copied().find(|&m| m as u32 <= minutes).unwrap_or(0)
    }

    fn boost_code(minutes: u8) -> u8 {
        BOOST_MINUTES.iter().rposition(|&m| m <= minutes).unwrap_or(0) as u8
    }

    /// `0x12`. Percentages are 0-100; boost valve position in steps of 5 %.
    pub fn config_valve(
        boost_minutes: u8,
        boost_valve_position: u8,
        decalc_day: Weekday,
        decalc_hour: u8,
        max_valve_setting: u8,
        valve_offset: u8,
    ) -> Vec<u8> {
        let pct = |v: u8| (v.min(100) as u16 * 255 / 100) as u8;
        alloc::vec![
            (boost_code(boost_minutes) << 5) | (boost_valve_position.min(100) / 5),
            (decalc_day.max_index() << 5) | decalc_hour.min(23),
            pct(max_valve_setting),
            pct(valve_offset),
        ]
    }

    /// `0x20` / `0x21`.
    pub fn link_partner(partner: Address, partner_type: DeviceType) -> Vec<u8> {
        let mut p = partner.0.to_vec();
        p.push(partner_type.raw());
        p
    }

    /// `0x22`.
    pub fn set_group(group: u8) -> Vec<u8> {
        alloc::vec![group]
    }

    /// `0x82`.
    pub fn display_actual_temperature(enabled: bool) -> Vec<u8> {
        alloc::vec![if enabled { 0x04 } else { 0x00 }]
    }

    /// `0x03`, local time. Same encoding as FHEM `CUL_MAX_GetTimeInformationPayload`.
    pub fn time_information(t: &LocalTime) -> Vec<u8> {
        alloc::vec![
            (t.year.saturating_sub(2000)) as u8,
            t.day,
            t.hour,
            t.minute | ((t.month & 0x0C) << 4),
            t.second | ((t.month & 0x03) << 6),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn rx_bytes(frame: &Frame) -> Vec<u8> {
        let mut raw = frame.encode();
        raw.push(0xC4); // RSSI raw -60 -> -104 dBm
        raw.push(0x80 | 0x2A); // CRC OK, LQI 42
        raw
    }

    fn frame(command: u8, payload: Vec<u8>) -> Frame {
        Frame {
            msgcnt: 7,
            flags: 0,
            command,
            src: Address([0x11, 0x22, 0x33]),
            dst: Address([0x12, 0x34, 0x56]),
            group: 0,
            payload,
        }
    }

    #[test]
    fn decode_roundtrip_and_status() {
        let f = frame(command::SET_TEMPERATURE, vec![0x2B]);
        let rx = decode_rx(&rx_bytes(&f)).unwrap();
        assert_eq!(rx.frame, f);
        assert_eq!(rx.lqi, 0x2A);
        assert_eq!(rx.rssi_dbm, -60 / 2 - 74);
    }

    #[test]
    fn decode_checks_real_crc_bit() {
        let mut raw = rx_bytes(&frame(command::ACK, vec![1]));
        *raw.last_mut().unwrap() &= 0x7F;
        assert_eq!(decode_rx(&raw), Err(DecodeError::Crc));
    }

    #[test]
    fn decode_rejects_length_mismatch_and_short() {
        let mut raw = rx_bytes(&frame(command::ACK, vec![1]));
        raw.push(0);
        assert!(matches!(decode_rx(&raw), Err(DecodeError::LengthMismatch { .. })));
        assert_eq!(decode_rx(&[3, 1, 2, 3, 0, 0x80]), Err(DecodeError::TooShort));
        assert_eq!(decode_rx(&[]), Err(DecodeError::Empty));
    }

    #[test]
    fn rssi_conversion() {
        assert_eq!(rssi_dbm(0), -74);
        assert_eq!(rssi_dbm(20), -64);
        assert_eq!(rssi_dbm(0xFF), -74); // -1/2 truncates to 0
        assert_eq!(rssi_dbm(0x80), -64 - 74);
    }

    #[test]
    fn thermostat_state_with_measured_temperature() {
        // manual, valve 40 %, desired 21.5, measured 0x0D4 = 212 -> 21.2
        let s = parse_thermostat_state(&[0x01, 40, 43, 0x00, 0xD4]).unwrap();
        assert_eq!(s.status.mode, Mode::Manual);
        assert_eq!(s.valve_position, 40);
        assert_eq!(s.desired, 21.5);
        assert_eq!(s.measured, Some(21.2));
    }

    #[test]
    fn thermostat_state_in_temporary_mode_has_no_measured_temperature() {
        // The old firmware decoded the "until" date here as a temperature.
        let s = parse_thermostat_state(&[0x02, 40, 43, 0x9D, 0x0B, 0x20]).unwrap();
        assert_eq!(s.status.mode, Mode::Temporary);
        assert_eq!(s.measured, None);
    }

    #[test]
    fn thermostat_state_from_ack_without_temperature() {
        let s = parse_thermostat_state(&[0x19, 0, 34]).unwrap();
        assert_eq!(s.status.mode, Mode::Manual);
        assert!(s.status.dst_auto);
        assert_eq!(s.desired, 17.0);
        assert_eq!(s.measured, None);
    }

    #[test]
    fn wall_thermostat_state_reads_its_own_sensor_like_fhem() {
        // status, display, desired 21.0 (+ bit 8 of temp), null1, heater temp, null2, temp
        // Own sensor: 0x100 | 0x04 = 260 -> 26.0. Heater temperature 0xCC must be ignored.
        let s = parse_wall_thermostat_state(&[0x01, 0x04, 0x80 | 42, 0x00, 0xCC, 0x00, 0x04]).unwrap();
        assert_eq!(s.desired, 21.0);
        assert!(s.display_actual_temperature);
        assert_eq!(s.measured, Some(26.0));
    }

    #[test]
    fn wall_thermostat_state_short_form_has_no_temperature() {
        let s = parse_wall_thermostat_state(&[0x00, 0x00, 42, 0x00, 0xCC]).unwrap();
        assert_eq!(s.measured, None);
        assert_eq!(s.status.mode, Mode::Auto);
    }

    #[test]
    fn wall_thermostat_control() {
        let m = parse(&frame(command::WALL_THERMOSTAT_CONTROL, vec![0x80 | 43, 0x04]));
        assert_eq!(m, Message::WallThermostatControl { desired: 21.5, measured: 26.0 });
    }

    #[test]
    fn ack_carries_state_and_nack_flag() {
        assert_eq!(
            parse(&frame(command::ACK, vec![0x01, 0x01, 0x20, 42])),
            Message::Ack { ok: true, state: vec![0x01, 0x20, 42] }
        );
        assert_eq!(parse(&frame(command::ACK, vec![0x81])), Message::Ack { ok: false, state: vec![] });
    }

    #[test]
    fn short_payloads_are_malformed_not_garbage() {
        assert_eq!(parse(&frame(command::THERMOSTAT_STATE, vec![0x01])), Message::Malformed);
        assert_eq!(parse(&frame(command::ACK, vec![])), Message::Malformed);
        assert_eq!(parse(&frame(0x99, vec![])), Message::Unknown);
    }

    #[test]
    fn pair_ping() {
        let mut p = vec![0x10, 0x03, 0xFF];
        p.extend_from_slice(b"KEQ0123456");
        assert_eq!(
            parse(&frame(command::PAIR_PING, p)),
            Message::PairPing {
                firmware: 0x10,
                device_type: Some(DeviceType::WallThermostat),
                serial: "KEQ0123456".into()
            }
        );
    }

    #[test]
    fn shutter_contact() {
        let m = parse(&frame(command::SHUTTER_CONTACT_STATE, vec![0x12]));
        assert!(matches!(m, Message::ShutterContactState { open: true, .. }));
        let m = parse(&frame(command::SHUTTER_CONTACT_STATE, vec![0x80]));
        assert!(matches!(m, Message::ShutterContactState { open: false, status: Status { low_battery: true, .. } }));
    }

    #[test]
    fn set_temperature_payload() {
        assert_eq!(payload::set_temperature(Mode::Manual, 21.5, None), vec![0x40 | 43]);
        assert_eq!(payload::set_temperature(Mode::Auto, 0.0, None), vec![0x00]);
        assert_eq!(payload::set_temperature(Mode::Boost, 0.0, None), vec![0xC0]);
    }

    #[test]
    fn until_encoding_matches_fhem() {
        // FHEM: (($month&0xE) << 20) | ($day << 16) | (($month&1) << 15) | (($year-2000) << 8) | ($hour*2 + int($min/30))
        let u = Until::parse("2026-11-15 18:30").unwrap();
        let v: u32 = ((11 & 0xE) << 20) | (15 << 16) | ((11 & 1) << 15) | (26 << 8) | 37;
        assert_eq!(u.encode(), [(v >> 16) as u8, (v >> 8) as u8, v as u8]);
        assert!(Until::parse("2026-11-15 18:15").is_none());
        assert!(Until::parse("2026-13-15 18:00").is_none());
    }

    #[test]
    fn config_valve_payload() {
        let p = payload::config_valve(30, 100, Weekday::Saturday, 12, 100, 0);
        assert_eq!(p, vec![(6 << 5) | 20, 12, 255, 0]);
        let p = payload::config_valve(60, 80, Weekday::Monday, 4, 50, 10);
        assert_eq!(p, vec![(7 << 5) | 16, (2 << 5) | 4, 127, 25]);
    }

    #[test]
    fn boost_minutes_round_down() {
        assert_eq!(payload::boost_minutes_supported(30), 30);
        assert_eq!(payload::boost_minutes_supported(45), 30);
        assert_eq!(payload::boost_minutes_supported(90), 60);
        assert_eq!(payload::boost_minutes_supported(7), 5);
        assert_eq!(payload::boost_minutes_supported(3), 0);
    }

    #[test]
    fn config_temperatures_payload() {
        let p = payload::config_temperatures(21.5, 17.0, 30.5, 4.5, 0.0, 12.0, 15);
        assert_eq!(p, vec![43, 34, 61, 9, 7, 24, 3]);
    }

    #[test]
    fn time_information_payload() {
        let t = LocalTime { year: 2026, month: 11, day: 15, hour: 8, minute: 33, second: 25, weekday: Weekday::Sunday };
        assert_eq!(payload::time_information(&t), vec![26, 15, 8, 33 | (8 << 4), 25 | (3 << 6)]);
    }
}
