//! Wall clock: UTC seconds to local time with EU daylight saving rules, without a tz database.

use alloc::string::String;

use serde::{Deserialize, Serialize};

/// Days of the week in MAX! order: the protocol numbers them from Saturday.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Weekday {
    Saturday,
    Sunday,
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
}

impl Weekday {
    pub const ALL: [Weekday; 7] = [
        Weekday::Saturday,
        Weekday::Sunday,
        Weekday::Monday,
        Weekday::Tuesday,
        Weekday::Wednesday,
        Weekday::Thursday,
        Weekday::Friday,
    ];

    pub fn max_index(self) -> u8 {
        self as u8
    }

    pub fn from_max_index(i: u8) -> Option<Self> {
        Self::ALL.get(i as usize).copied()
    }

    pub fn name(self) -> &'static str {
        match self {
            Weekday::Saturday => "saturday",
            Weekday::Sunday => "sunday",
            Weekday::Monday => "monday",
            Weekday::Tuesday => "tuesday",
            Weekday::Wednesday => "wednesday",
            Weekday::Thursday => "thursday",
            Weekday::Friday => "friday",
        }
    }

    /// Accepts full names and three-letter abbreviations, any case.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        Self::ALL.into_iter().find(|d| {
            let name = d.name();
            s.eq_ignore_ascii_case(name) || (s.len() == 3 && s.eq_ignore_ascii_case(&name[..3]))
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TimeZone {
    /// Standard (winter) offset from UTC. Central Europe: 60.
    pub utc_offset_minutes: i16,
    /// Apply EU summer time: +1 h from the last Sunday of March to the last Sunday of
    /// October, switching at 01:00 UTC.
    pub eu_dst: bool,
}

impl Default for TimeZone {
    fn default() -> Self {
        TimeZone { utc_offset_minutes: 60, eu_dst: true }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    pub weekday: Weekday,
}

impl LocalTime {
    pub fn iso(&self) -> String {
        alloc::format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            self.year,
            self.month,
            self.day,
            self.hour,
            self.minute,
            self.second
        )
    }
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's algorithm).
pub fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = month as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Inverse of [`days_from_civil`].
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + if month <= 2 { 1 } else { 0 };
    (year, month, day)
}

/// 0 = Sunday.
fn weekday_sun0(days: i64) -> i64 {
    (days + 4).rem_euclid(7)
}

fn last_sunday(year: i64, month: u32) -> i64 {
    let next_month_first =
        if month == 12 { days_from_civil(year + 1, 1, 1) } else { days_from_civil(year, month + 1, 1) };
    let last = next_month_first - 1;
    last - weekday_sun0(last)
}

fn is_eu_summer_time(utc: i64) -> bool {
    let (year, _, _) = civil_from_days(utc.div_euclid(86_400));
    let start = last_sunday(year, 3) * 86_400 + 3_600;
    let end = last_sunday(year, 10) * 86_400 + 3_600;
    (start..end).contains(&utc)
}

pub fn local_time(unix_utc: u64, tz: &TimeZone) -> LocalTime {
    let utc = unix_utc as i64;
    let dst = tz.eu_dst && is_eu_summer_time(utc);
    let local = utc + tz.utc_offset_minutes as i64 * 60 + if dst { 3_600 } else { 0 };
    let days = local.div_euclid(86_400);
    let secs = local.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let weekday = Weekday::from_max_index(((weekday_sun0(days) + 1) % 7) as u8).unwrap_or(Weekday::Saturday);
    LocalTime {
        year: year as u16,
        month: month as u8,
        day: day as u8,
        hour: (secs / 3_600) as u8,
        minute: (secs % 3_600 / 60) as u8,
        second: (secs % 60) as u8,
        weekday,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CET: TimeZone = TimeZone { utc_offset_minutes: 60, eu_dst: true };

    fn utc(y: i64, mo: u32, d: u32, h: i64, mi: i64) -> u64 {
        (days_from_civil(y, mo, d) * 86_400 + h * 3_600 + mi * 60) as u64
    }

    #[test]
    fn civil_roundtrip() {
        for z in [-1000, 0, 1, 365, 10_957, 20_407, 50_000] {
            let (y, m, d) = civil_from_days(z);
            assert_eq!(days_from_civil(y, m, d), z);
        }
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
    }

    #[test]
    fn winter_and_summer_offsets() {
        let t = local_time(utc(2026, 1, 10, 12, 0), &CET);
        assert_eq!((t.hour, t.weekday), (13, Weekday::Saturday));
        let t = local_time(utc(2026, 7, 1, 12, 0), &CET);
        assert_eq!((t.hour, t.weekday), (14, Weekday::Wednesday));
    }

    #[test]
    fn dst_switches_at_one_utc_on_last_sundays() {
        // 2026: summer time starts Sun 29 March, ends Sun 25 October.
        assert_eq!(local_time(utc(2026, 3, 29, 0, 59), &CET).hour, 1);
        assert_eq!(local_time(utc(2026, 3, 29, 1, 0), &CET).hour, 3);
        assert_eq!(local_time(utc(2026, 10, 25, 0, 59), &CET).hour, 2);
        assert_eq!(local_time(utc(2026, 10, 25, 1, 0), &CET).hour, 2);
        assert_eq!(local_time(utc(2026, 10, 25, 1, 0), &CET).minute, 0);
    }

    #[test]
    fn date_rollover_and_iso() {
        let t = local_time(utc(2026, 12, 31, 23, 30), &CET);
        assert_eq!(t.iso(), "2027-01-01 00:30:00");
        assert_eq!(t.weekday, Weekday::Friday);
    }

    #[test]
    fn no_dst_zone() {
        let tz = TimeZone { utc_offset_minutes: 0, eu_dst: false };
        assert_eq!(local_time(utc(2026, 7, 1, 12, 0), &tz).hour, 12);
    }

    #[test]
    fn weekday_names() {
        assert_eq!(Weekday::parse(" Monday "), Some(Weekday::Monday));
        assert_eq!(Weekday::parse("sat"), Some(Weekday::Saturday));
        assert_eq!(Weekday::parse("someday"), None);
        assert_eq!(Weekday::Monday.max_index(), 2);
    }
}
