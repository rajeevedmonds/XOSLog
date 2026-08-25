//! Wall-clock timestamps implemented in pure Rust.
//!
//! [`std::time::SystemTime`] only exposes a duration since the Unix epoch, so
//! the calendar conversion is implemented here from scratch using Howard
//! Hinnant's public-domain `civil_from_days` algorithm. No libc calls and no
//! dependencies are needed.

use std::fmt;

const MICROS_PER_SECOND: i128 = 1_000_000;
const MICROS_PER_MINUTE: i128 = 60_000_000;
const MICROS_PER_HOUR: i128 = 3_600_000_000;
const MICROS_PER_DAY: i128 = 86_400_000_000;

/// A broken-down wall-clock time with microsecond precision and an explicit
/// UTC offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timestamp {
    /// Calendar year, e.g. `2026`.
    pub year: i32,
    /// Calendar month, 1-12.
    pub month: u8,
    /// Day of month, 1-31.
    pub day: u8,
    /// Hour of day, 0-23.
    pub hour: u8,
    /// Minute of hour, 0-59.
    pub minute: u8,
    /// Second of minute, 0-59.
    pub second: u8,
    /// Microsecond within the second, 0-999_999.
    pub microsecond: u32,
    /// Offset from UTC in seconds. `0` means UTC.
    pub offset_seconds: i32,
}

impl Timestamp {
    /// The current wall-clock time shifted by `offset_seconds` relative to
    /// UTC. Pass `0` for UTC, or e.g. `19800` for UTC+05:30.
    #[must_use]
    pub fn now(offset_seconds: i32) -> Timestamp {
        let micros = unix_micros() + i128::from(offset_seconds) * MICROS_PER_SECOND;
        from_unix_micros(micros, offset_seconds)
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:06}",
            self.year, self.month, self.day, self.hour, self.minute, self.second, self.microsecond
        )?;
        if self.offset_seconds == 0 {
            f.write_str("Z")
        } else {
            let sign = if self.offset_seconds < 0 { '-' } else { '+' };
            let abs = self.offset_seconds.unsigned_abs();
            write!(f, "{}{:02}:{:02}", sign, abs / 3600, (abs % 3600) / 60)
        }
    }
}

/// Signed microseconds since the Unix epoch.
fn unix_micros() -> i128 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i128 * MICROS_PER_SECOND + i128::from(d.subsec_nanos()) / 1000,
        Err(e) => {
            let d = e.duration();
            -(d.as_secs() as i128) * MICROS_PER_SECOND - i128::from(d.subsec_nanos()) / 1000
        }
    }
}

/// Decompose a signed microsecond count into calendar fields.
fn from_unix_micros(micros: i128, offset_seconds: i32) -> Timestamp {
    let days = micros.div_euclid(MICROS_PER_DAY);
    let rem = micros.rem_euclid(MICROS_PER_DAY);
    let hour = (rem / MICROS_PER_HOUR) as u8;
    let minute = ((rem % MICROS_PER_HOUR) / MICROS_PER_MINUTE) as u8;
    let second = ((rem % MICROS_PER_MINUTE) / MICROS_PER_SECOND) as u8;
    let microsecond = (rem % MICROS_PER_SECOND) as u32;
    let (year, month, day) = civil_from_days(days);
    Timestamp {
        year,
        month,
        day,
        hour,
        minute,
        second,
        microsecond,
        offset_seconds,
    }
}

/// Convert a count of days since the Unix epoch into a civil (Gregorian)
/// date, correctly handling negative day counts.
///
/// Based on Howard Hinnant's public-domain `civil_from_days` algorithm.
fn civil_from_days(days: i128) -> (i32, u8, u8) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    let year = if month <= 2 { year + 1 } else { year };
    (year as i32, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_is_utc_midnight() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
    }

    #[test]
    fn day_before_epoch() {
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
    }

    #[test]
    fn leap_day() {
        // Days since epoch to 2024-02-29.
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
    }

    #[test]
    fn display_utc() {
        let ts = Timestamp {
            year: 2026,
            month: 8,
            day: 13,
            hour: 4,
            minute: 49,
            second: 0,
            microsecond: 123_456,
            offset_seconds: 0,
        };
        assert_eq!(ts.to_string(), "2026-08-13T04:49:00.123456Z");
    }

    #[test]
    fn display_offset() {
        let ts = Timestamp {
            year: 2026,
            month: 8,
            day: 13,
            hour: 10,
            minute: 19,
            second: 0,
            microsecond: 0,
            offset_seconds: 19_800,
        };
        assert_eq!(ts.to_string(), "2026-08-13T10:19:00.000000+05:30");
    }

    #[test]
    fn negative_offset() {
        let ts = Timestamp {
            year: 2026,
            month: 8,
            day: 12,
            hour: 20,
            minute: 49,
            second: 0,
            microsecond: 0,
            offset_seconds: -28_800,
        };
        assert_eq!(ts.to_string(), "2026-08-12T20:49:00.000000-08:00");
    }

    #[test]
    fn now_formats_cleanly() {
        let s = Timestamp::now(0).to_string();
        assert_eq!(s.len(), 27);
        assert!(s.ends_with('Z'));
        let bytes = s.as_bytes();
        assert_eq!(&bytes[4..5], b"-");
        assert_eq!(&bytes[7..8], b"-");
        assert_eq!(&bytes[10..11], b"T");
        assert_eq!(&bytes[13..14], b":");
        assert_eq!(&bytes[16..17], b":");
        assert_eq!(&bytes[19..20], b".");
    }

    #[test]
    fn offset_shifts_date() {
        let ts = Timestamp::now(3600).to_string();
        assert!(ts.ends_with("+01:00"), "expected +01:00 offset, got: {ts}");
    }
}
