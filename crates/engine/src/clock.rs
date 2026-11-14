//! Wall-clock time for the scheduler, behind a trait so tests can drive it.
//!
//! The scheduler works in the daemon machine's local wall-clock time at
//! minute resolution ([`LocalTime`]): queue start/stop times and speed
//! windows are `"HH:MM"` on days of the week. It never sleeps until a
//! computed instant; it looks at the clock every [`Tuning::clock_tick`]
//! and acts on what the wall clock crossed since the last look (see
//! `schedule.rs`). That makes time-zone changes, DST and a machine waking
//! from sleep plain cases of "the wall clock jumped".
//!
//! [`SystemClock`] reads the local time zone with `jiff`, which follows the
//! IANA database on Unix (`/etc/localtime`, `TZ`) and maps the Windows zone
//! to it (bundling the database there, as Windows has none), and notices a
//! changed zone within five minutes.
//!
//! [`Tuning::clock_tick`]: crate::Tuning::clock_tick

use std::fmt;

use whoosh_proto::UnixMillis;

/// A local wall-clock time at minute resolution: `day` counts days from
/// 1970-01-01 in the local calendar (not UTC), `minute` is 0–1439.
/// Ordered by `(day, minute)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LocalTime {
    pub day: i64,
    pub minute: u32,
}

impl LocalTime {
    pub const MINUTES_PER_DAY: u32 = 24 * 60;

    /// From a civil date and time (`month` 1–12, `day` 1–31).
    pub fn from_civil(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> Self {
        Self {
            day: days_from_civil(year, month, day),
            minute: hour * 60 + minute,
        }
    }

    /// From minutes since 1970-01-01 00:00 local time.
    pub fn from_minutes(minutes: i64) -> Self {
        Self {
            day: minutes.div_euclid(i64::from(Self::MINUTES_PER_DAY)),
            minute: minutes.rem_euclid(i64::from(Self::MINUTES_PER_DAY)) as u32,
        }
    }

    /// Minutes since 1970-01-01 00:00 local time.
    pub fn minutes(self) -> i64 {
        self.day * i64::from(Self::MINUTES_PER_DAY) + i64::from(self.minute)
    }

    /// 0 = Sunday … 6 = Saturday (PROTOCOL.md's numbering).
    pub fn weekday(self) -> u8 {
        weekday(self.day)
    }
}

impl fmt::Display for LocalTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "day {} {:02}:{:02}",
            self.day,
            self.minute / 60,
            self.minute % 60
        )
    }
}

/// 0 = Sunday … 6 = Saturday of the day `day` days after 1970-01-01 (a
/// Thursday).
pub(crate) fn weekday(day: i64) -> u8 {
    (day + 4).rem_euclid(7) as u8
}

/// Days from 1970-01-01 to a proleptic Gregorian date (H. Hinnant's
/// `days_from_civil`).
fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let y = i64::from(year) - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(month);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The wall clock the scheduler reads. [`SystemClock`] in the daemon; a
/// fake one in tests.
pub trait Clock: Send + Sync + fmt::Debug {
    /// Milliseconds since the Unix epoch (UTC), for `event.queue.finished`'s
    /// `atMs`.
    fn now_ms(&self) -> UnixMillis;
    /// The local wall-clock time now.
    fn local(&self) -> LocalTime;
}

/// The machine's clock and time zone.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> UnixMillis {
        crate::util::unix_millis()
    }

    fn local(&self) -> LocalTime {
        let z = jiff::Zoned::now();
        LocalTime::from_civil(
            i32::from(z.year()),
            z.month() as u32,
            z.day() as u32,
            z.hour() as u32,
            z.minute() as u32,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_days_and_weekdays() {
        assert_eq!(LocalTime::from_civil(1970, 1, 1, 0, 0).day, 0);
        assert_eq!(LocalTime::from_civil(1970, 1, 1, 0, 0).weekday(), 4);
        let t = LocalTime::from_civil(2026, 11, 14, 23, 5);
        assert_eq!(t.weekday(), 6, "2026-11-14 is a Saturday");
        assert_eq!(t.minute, 23 * 60 + 5);
        assert_eq!(LocalTime::from_civil(2000, 3, 1, 0, 0).day, 11_017);
        assert_eq!(LocalTime::from_civil(1969, 12, 31, 0, 0).day, -1);
        assert_eq!(LocalTime::from_civil(1969, 12, 31, 0, 0).weekday(), 3);
        assert_eq!(LocalTime::from_minutes(t.minutes()), t);
        assert_eq!(LocalTime::from_minutes(-1).minute, 1439);
    }

    #[test]
    fn the_system_clock_agrees_with_utc_roughly() {
        let c = SystemClock;
        let local = c.local().minutes();
        let utc = c.now_ms() / 60_000;
        // Every zone is within 14 hours of UTC.
        assert!((local - utc).abs() <= 14 * 60 + 1, "{local} vs {utc}");
    }
}
