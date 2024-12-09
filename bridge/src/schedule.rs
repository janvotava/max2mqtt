//! Day schedules (week profile).
//!
//! On the wire a day is up to 13 switch points of "temperature UNTIL time", the last one
//! ending at 24:00, sent as two messages of 7 + 6 points (FHEM `MAX_Set` weekProfile).
//!
//! The MQTT API takes start times instead, because that is how people write schedules and
//! how the README example reads: `{"6:00": 21.5, "22:30": 4.5}` means 21.5 °C from 6:00 and
//! 4.5 °C from 22:30, which wraps around to the morning. The old firmware sent those keys as
//! "until" times without the closing 24:00 point, so that example programmed 21.5 °C until
//! 6:00, 4.5 °C until 22:30, and left the rest of the day to whatever was stored before.

use alloc::string::String;
use alloc::vec::Vec;

use crate::protocol::{TEMPERATURE_MAX, TEMPERATURE_MIN, half_degrees};

pub const MAX_POINTS: usize = 13;
pub const END_OF_DAY: u16 = 24 * 60;

/// Filler for unused points: 17 °C until 24:00, as FHEM sends.
const FILLER: [u8; 2] = [0x45, 0x20];

#[derive(Clone, Debug, PartialEq)]
pub enum ScheduleError {
    Empty,
    TooManyPoints(usize),
    BadTime(String),
    BadTemperature(f32),
    DuplicateTime(u16),
}

impl core::fmt::Display for ScheduleError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Empty => f.write_str("schedule is empty"),
            Self::TooManyPoints(n) => write!(f, "schedule needs {n} switch points, devices hold {MAX_POINTS}"),
            Self::BadTime(t) => write!(f, "invalid time {t:?}, expected H:MM in 5 minute steps"),
            Self::BadTemperature(t) => {
                write!(f, "invalid temperature {t}, expected {TEMPERATURE_MIN}-{TEMPERATURE_MAX}")
            }
            Self::DuplicateTime(m) => write!(f, "time {} given twice", format_time(*m)),
        }
    }
}

/// Parse `"H:MM"` or `"HH:MM"` into minutes after midnight. Must be a 5 minute step.
pub fn parse_time(s: &str) -> Result<u16, ScheduleError> {
    let bad = || ScheduleError::BadTime(s.into());
    let (h, m) = s.trim().split_once(':').ok_or_else(bad)?;
    let h: u16 = h.parse().map_err(|_| bad())?;
    let m: u16 = m.parse().map_err(|_| bad())?;
    if h > 23 || m > 59 || !m.is_multiple_of(5) {
        return Err(bad());
    }
    Ok(h * 60 + m)
}

pub fn format_time(minutes: u16) -> String {
    alloc::format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SwitchPoint {
    /// Minutes after midnight, 5..=1440.
    pub until: u16,
    pub half_degrees: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DaySchedule {
    pub points: Vec<SwitchPoint>,
}

impl DaySchedule {
    /// Build from (start minute, °C) pairs in any order.
    pub fn from_start_times(entries: impl IntoIterator<Item = (u16, f32)>) -> Result<Self, ScheduleError> {
        let mut starts: Vec<(u16, u8)> = Vec::new();
        for (minute, celsius) in entries {
            if !(TEMPERATURE_MIN..=TEMPERATURE_MAX).contains(&celsius) {
                return Err(ScheduleError::BadTemperature(celsius));
            }
            starts.push((minute, half_degrees(celsius)));
        }
        starts.sort_by_key(|&(m, _)| m);
        if let Some(w) = starts.windows(2).find(|w| w[0].0 == w[1].0) {
            return Err(ScheduleError::DuplicateTime(w[0].0));
        }
        let (Some(&(first_start, _)), Some(&(_, last_temp))) = (starts.first(), starts.last()) else {
            return Err(ScheduleError::Empty);
        };

        let mut points = Vec::with_capacity(starts.len() + 1);
        if first_start != 0 {
            // From midnight, the evening's temperature continues.
            points.push(SwitchPoint { until: first_start, half_degrees: last_temp });
        }
        for (i, &(_, temp)) in starts.iter().enumerate() {
            let until = starts.get(i + 1).map_or(END_OF_DAY, |&(next, _)| next);
            match points.last_mut() {
                Some(prev) if prev.half_degrees == temp => prev.until = until,
                _ => points.push(SwitchPoint { until, half_degrees: temp }),
            }
        }
        if points.len() > MAX_POINTS {
            return Err(ScheduleError::TooManyPoints(points.len()));
        }
        Ok(DaySchedule { points })
    }

    /// 13 big-endian 16 bit points: `temperature*2 << 9 | until/5`, unused ones filled.
    pub fn encode(&self) -> [u8; MAX_POINTS * 2] {
        let mut out = [0u8; MAX_POINTS * 2];
        for (i, chunk) in out.as_chunks_mut::<2>().0.iter_mut().enumerate() {
            let bytes = match self.points.get(i) {
                Some(p) => (((p.half_degrees as u16) << 9) | (p.until / 5)).to_be_bytes(),
                None => FILLER,
            };
            chunk.copy_from_slice(&bytes);
        }
        out
    }

    /// `ConfigWeekProfile` payloads: day byte (`0x10` set on the second part) + points.
    pub fn payloads(&self, max_day: u8) -> Vec<Vec<u8>> {
        let encoded = self.encode();
        let mut first = alloc::vec![max_day];
        first.extend_from_slice(&encoded[..14]);
        let mut out = alloc::vec![first];
        if self.points.len() > 7 {
            let mut second = alloc::vec![0x10 | max_day];
            second.extend_from_slice(&encoded[14..]);
            out.push(second);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn pt(until: u16, celsius: f32) -> SwitchPoint {
        SwitchPoint { until, half_degrees: half_degrees(celsius) }
    }

    #[test]
    fn readme_example_wraps_evening_into_morning() {
        let s =
            DaySchedule::from_start_times([(parse_time("6:00").unwrap(), 21.5), (parse_time("22:30").unwrap(), 4.5)])
                .unwrap();
        assert_eq!(s.points, vec![pt(360, 4.5), pt(1350, 21.5), pt(1440, 4.5)]);
    }

    #[test]
    fn starting_at_midnight_needs_no_wrap() {
        let s = DaySchedule::from_start_times([(0, 18.0), (420, 21.0)]).unwrap();
        assert_eq!(s.points, vec![pt(420, 18.0), pt(1440, 21.0)]);
    }

    #[test]
    fn single_temperature_is_one_point() {
        let s = DaySchedule::from_start_times([(600, 20.0)]).unwrap();
        assert_eq!(s.points, vec![pt(1440, 20.0)]);
    }

    #[test]
    fn order_of_input_does_not_matter() {
        let a = DaySchedule::from_start_times([(1350, 4.5), (360, 21.5)]).unwrap();
        let b = DaySchedule::from_start_times([(360, 21.5), (1350, 4.5)]).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn rejects_bad_input() {
        assert_eq!(DaySchedule::from_start_times([]), Err(ScheduleError::Empty));
        assert_eq!(DaySchedule::from_start_times([(0, 31.0)]), Err(ScheduleError::BadTemperature(31.0)));
        assert_eq!(DaySchedule::from_start_times([(5, 20.0), (5, 21.0)]), Err(ScheduleError::DuplicateTime(5)));
        let many: Vec<(u16, f32)> = (0..14).map(|i| (i * 60, if i % 2 == 0 { 20.0 } else { 18.0 })).collect();
        assert!(matches!(DaySchedule::from_start_times(many), Err(ScheduleError::TooManyPoints(14))));
        assert!(parse_time("6:03").is_err());
        assert!(parse_time("24:00").is_err());
        assert!(parse_time("six").is_err());
        assert_eq!(parse_time("06:05"), Ok(365));
    }

    #[test]
    fn encoding_matches_fhem() {
        // FHEM: sprintf("%04x", (int($temperature*2) << 9) | int(($hour * 60 + $min)/5))
        let s = DaySchedule::from_start_times([(0, 21.5)]).unwrap();
        let e = s.encode();
        assert_eq!(&e[..2], &((43u16 << 9) | 288).to_be_bytes());
        assert_eq!(&e[2..4], &FILLER);
    }

    #[test]
    fn short_schedule_is_one_message_long_one_is_two() {
        let short = DaySchedule::from_start_times([(360, 21.0), (1320, 17.0)]).unwrap();
        let p = short.payloads(2);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].len(), 15);
        assert_eq!(p[0][0], 2);

        let entries: Vec<(u16, f32)> = (0..10).map(|i| (i * 120, if i % 2 == 0 { 20.0 } else { 18.0 })).collect();
        let long = DaySchedule::from_start_times(entries).unwrap();
        let p = long.payloads(2);
        assert_eq!(p.len(), 2);
        assert_eq!(p[1][0], 0x12);
        assert_eq!(p[1].len(), 13);
    }
}
