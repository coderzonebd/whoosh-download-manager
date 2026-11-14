//! The time rules of the scheduler, as pure functions over [`LocalTime`]:
//! when a queue's scheduled start or stop is due, and which speed limit
//! applies now.
//!
//! **Edges, not levels.** A queue's `start` and `stop` are events ("at
//! 01:00 the queue runs"), not a window the queue is forced into: a user who
//! starts or stops a queue by hand keeps that until the next event. Every
//! clock tick, [`Crossings::advance`] reports the events whose local time
//! lies in `(last look, now]`:
//!
//! - **Forward jumps** (a DST gap, a machine waking from sleep, a time zone
//!   further east, the clock set forward): events in the skipped time are
//!   not lost. Of the events a queue missed, only the latest is applied —
//!   after sleeping through 01:00 (start) and 07:00 (stop), the queue ends
//!   up stopped, without starting its downloads for an instant. At most
//!   [`MAX_CATCH_UP_DAYS`] are looked back.
//! - **Backward jumps** (DST ending, a time zone further west, the clock
//!   set back): nothing fires while the clock re-covers time it already
//!   showed, because an event fires at most once per local date.
//! - **The daemon was not running**: nothing is replayed at start; the
//!   queue keeps the `running` it had (it is persisted).
//!
//! **Speed windows** are levels: [`speed_limit`] is evaluated every tick
//! and whenever the settings change. A window is `[from, to)` on the days
//! it opens; `from` after `to` runs past midnight into the next day, and
//! `from` equal to `to` is a whole day from `from`.

use std::collections::HashSet;

use whoosh_proto::queue::is_clock_time;
use whoosh_proto::{ClockTime, Schedule, SpeedWindow};

use crate::clock::{LocalTime, weekday};

/// The furthest back a forward jump is caught up.
pub(crate) const MAX_CATCH_UP_DAYS: i64 = 7;

/// `"HH:MM"` → minutes since midnight.
pub(crate) fn parse_hhmm(s: &str) -> Option<u32> {
    if !is_clock_time(s) {
        return None;
    }
    let h: u32 = s[..2].parse().ok()?;
    let m: u32 = s[3..].parse().ok()?;
    Some(h * 60 + m)
}

/// Whether `weekday` is one of `days` (empty = every day).
fn on_day(days: &[u8], weekday: u8) -> bool {
    days.is_empty() || days.contains(&weekday)
}

pub(crate) fn check_days(days: &[u8]) -> Result<(), String> {
    if days.iter().any(|d| *d > 6) {
        return Err("days are 0 (Sunday) to 6".into());
    }
    Ok(())
}

pub(crate) fn check_schedule(s: &Schedule) -> Result<(), String> {
    for t in [&s.start, &s.stop].into_iter().flatten() {
        if !is_clock_time(&t.at) {
            return Err(format!("schedule time {:?} is not HH:MM", t.at));
        }
        check_days(&t.days)?;
    }
    Ok(())
}

pub(crate) fn check_windows(windows: &[SpeedWindow]) -> Result<(), String> {
    for w in windows {
        if !is_clock_time(&w.from) || !is_clock_time(&w.to) {
            return Err("speed window times must be HH:MM".into());
        }
        if w.limit_bps == 0 {
            return Err("speed window limitBps must be at least 1".into());
        }
        check_days(&w.days)?;
    }
    Ok(())
}

/// Whether `w` is open at `now`.
pub(crate) fn window_open(w: &SpeedWindow, now: LocalTime) -> bool {
    let (Some(from), Some(to)) = (parse_hhmm(&w.from), parse_hhmm(&w.to)) else {
        return false;
    };
    let today = now.weekday();
    let yesterday = weekday(now.day - 1);
    let m = now.minute;
    if from < to {
        on_day(&w.days, today) && (from..to).contains(&m)
    } else {
        // Over midnight (or a whole day when from == to): the part from
        // `from` today, and the part before `to` of a window that opened
        // yesterday.
        (on_day(&w.days, today) && m >= from) || (on_day(&w.days, yesterday) && m < to)
    }
}

/// The speed limit in force at `now`: the lowest limit of the open
/// windows, else `base` (`Settings::speed_limit_bps`).
pub(crate) fn speed_limit(
    windows: &[SpeedWindow],
    now: LocalTime,
    base: Option<u64>,
) -> Option<u64> {
    windows
        .iter()
        .filter(|w| w.limit_bps > 0 && window_open(w, now))
        .map(|w| w.limit_bps)
        .min()
        .or(base)
}

/// A scheduled queue event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Edge {
    Start,
    Stop,
}

/// Which scheduled events the wall clock crossed (see the module docs).
#[derive(Debug, Default)]
pub(crate) struct Crossings {
    last: Option<LocalTime>,
    /// `(queue, edge, local day)` already fired: at most once per date.
    fired: HashSet<(String, Edge, i64)>,
}

impl Crossings {
    /// Look at the clock: for every queue whose start or stop the clock
    /// crossed since the last look, the latest such event.
    pub(crate) fn advance<'a>(
        &mut self,
        now: LocalTime,
        queues: impl IntoIterator<Item = (&'a str, &'a Schedule)>,
    ) -> Vec<(String, Edge)> {
        let Some(last) = self.last.replace(now) else {
            return Vec::new();
        };
        if now <= last {
            return Vec::new();
        }
        let floor = LocalTime::from_minutes(
            now.minutes() - MAX_CATCH_UP_DAYS * i64::from(LocalTime::MINUTES_PER_DAY),
        );
        let from = last.max(floor);
        let mut out = Vec::new();
        for (name, schedule) in queues {
            let mut latest: Option<(LocalTime, Edge)> = None;
            let edges = [(Edge::Start, &schedule.start), (Edge::Stop, &schedule.stop)];
            for (edge, at) in edges {
                let Some(ClockTime { at, days }) = at else {
                    continue;
                };
                let Some(minute) = parse_hhmm(at) else {
                    continue;
                };
                for day in from.day..=now.day {
                    let t = LocalTime { day, minute };
                    if t <= from || t > now || !on_day(days, weekday(day)) {
                        continue;
                    }
                    if !self.fired.insert((name.to_string(), edge, day)) {
                        continue;
                    }
                    // Equal times: stopping is the safer reading.
                    if latest.is_none_or(|(lt, le)| t > lt || (t == lt && le == Edge::Start)) {
                        latest = Some((t, edge));
                    }
                }
            }
            if let Some((_, edge)) = latest {
                out.push((name.to_string(), edge));
            }
        }
        // Forget dates that can no longer be crossed again by DST (a day
        // back is enough; a clock set back further re-fires, which is what
        // the user asked for by doing it).
        self.fired.retain(|(_, _, day)| *day >= now.day - 1);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(day: i64, h: u32, m: u32) -> LocalTime {
        LocalTime {
            day,
            minute: h * 60 + m,
        }
    }

    fn win(days: &[u8], from: &str, to: &str, limit: u64) -> SpeedWindow {
        SpeedWindow {
            days: days.to_vec(),
            from: from.into(),
            to: to.into(),
            limit_bps: limit,
        }
    }

    fn sched(start: Option<(&str, &[u8])>, stop: Option<(&str, &[u8])>) -> Schedule {
        let t = |(a, d): (&str, &[u8])| ClockTime {
            at: a.into(),
            days: d.to_vec(),
        };
        Schedule {
            start: start.map(t),
            stop: stop.map(t),
            after_done: Default::default(),
        }
    }

    #[test]
    fn parses_clock_times() {
        assert_eq!(parse_hhmm("00:00"), Some(0));
        assert_eq!(parse_hhmm("23:59"), Some(1439));
        assert_eq!(parse_hhmm("24:00"), None);
        assert_eq!(parse_hhmm("7:00"), None);
    }

    // Day 3 (1970-01-04) is a Sunday.
    const SUN: i64 = 3;

    #[test]
    fn windows_within_a_day() {
        let w = win(&[], "09:00", "17:00", 100);
        assert!(!window_open(&w, at(SUN, 8, 59)));
        assert!(window_open(&w, at(SUN, 9, 0)));
        assert!(window_open(&w, at(SUN, 16, 59)));
        assert!(!window_open(&w, at(SUN, 17, 0)), "to is exclusive");
        let mon = win(&[1], "09:00", "17:00", 100);
        assert!(!window_open(&mon, at(SUN, 10, 0)));
        assert!(window_open(&mon, at(SUN + 1, 10, 0)));
    }

    #[test]
    fn windows_over_midnight_belong_to_the_day_they_open() {
        // Friday 22:00 to Saturday 06:00 only.
        let fri = win(&[5], "22:00", "06:00", 100);
        let friday = SUN + 5;
        assert!(!window_open(&fri, at(friday, 21, 59)));
        assert!(window_open(&fri, at(friday, 22, 0)));
        assert!(window_open(&fri, at(friday, 23, 59)));
        assert!(window_open(&fri, at(friday + 1, 0, 0)));
        assert!(window_open(&fri, at(friday + 1, 5, 59)));
        assert!(!window_open(&fri, at(friday + 1, 6, 0)));
        // Not Thursday night, nor Friday morning.
        assert!(!window_open(&fri, at(friday - 1, 23, 0)));
        assert!(!window_open(&fri, at(friday, 1, 0)));
        // Every day.
        let night = win(&[], "23:00", "01:00", 100);
        assert!(window_open(&night, at(SUN, 0, 30)));
        assert!(window_open(&night, at(SUN, 23, 30)));
        assert!(!window_open(&night, at(SUN, 1, 0)));
        // from == to: a whole day from `from`.
        let day = win(&[1], "12:00", "12:00", 100);
        assert!(!window_open(&day, at(SUN + 1, 11, 59)));
        assert!(window_open(&day, at(SUN + 1, 12, 0)));
        assert!(window_open(&day, at(SUN + 2, 11, 59)));
        assert!(!window_open(&day, at(SUN + 2, 12, 0)));
    }

    #[test]
    fn the_lowest_open_limit_replaces_the_base() {
        let ws = vec![
            win(&[], "08:00", "18:00", 500),
            win(&[], "12:00", "13:00", 200),
            win(&[], "22:00", "02:00", 900),
        ];
        assert_eq!(speed_limit(&ws, at(SUN, 7, 0), Some(50)), Some(50));
        assert_eq!(speed_limit(&ws, at(SUN, 7, 0), None), None);
        assert_eq!(speed_limit(&ws, at(SUN, 9, 0), None), Some(500));
        assert_eq!(speed_limit(&ws, at(SUN, 12, 30), Some(50)), Some(200));
        // A window's limit replaces the base even when it is higher.
        assert_eq!(speed_limit(&ws, at(SUN, 23, 0), Some(50)), Some(900));
        assert_eq!(speed_limit(&ws, at(SUN, 1, 59), Some(50)), Some(900));
        assert_eq!(speed_limit(&ws, at(SUN, 2, 0), Some(50)), Some(50));
    }

    #[test]
    fn validation() {
        assert!(check_windows(&[win(&[], "09:00", "17:00", 1)]).is_ok());
        assert!(check_windows(&[win(&[], "9:00", "17:00", 1)]).is_err());
        assert!(check_windows(&[win(&[], "09:00", "17:00", 0)]).is_err());
        assert!(check_windows(&[win(&[7], "09:00", "17:00", 1)]).is_err());
        assert!(check_schedule(&sched(Some(("01:00", &[0, 6])), None)).is_ok());
        assert!(check_schedule(&sched(Some(("25:00", &[])), None)).is_err());
        assert!(check_schedule(&sched(None, Some(("01:00", &[9])))).is_err());
    }

    #[test]
    fn edges_fire_once_as_the_clock_passes() {
        let s = sched(Some(("01:00", &[])), Some(("07:00", &[])));
        let q = || [("night", &s)];
        let mut c = Crossings::default();
        assert!(c.advance(at(SUN, 0, 58), q()).is_empty(), "first look");
        assert!(c.advance(at(SUN, 0, 59), q()).is_empty());
        assert_eq!(
            c.advance(at(SUN, 1, 0), q()),
            vec![("night".into(), Edge::Start)]
        );
        assert!(c.advance(at(SUN, 1, 0), q()).is_empty(), "same minute");
        assert!(c.advance(at(SUN, 6, 59), q()).is_empty());
        assert_eq!(
            c.advance(at(SUN, 7, 0), q()),
            vec![("night".into(), Edge::Stop)]
        );
        assert_eq!(
            c.advance(at(SUN + 1, 1, 0), q()),
            vec![("night".into(), Edge::Start)],
            "next day"
        );
    }

    #[test]
    fn days_limit_edges() {
        // Weekdays only.
        let s = sched(Some(("01:00", &[1, 2, 3, 4, 5])), None);
        let mut c = Crossings::default();
        c.advance(at(SUN, 0, 0), [("q", &s)]);
        assert!(c.advance(at(SUN, 2, 0), [("q", &s)]).is_empty(), "Sunday");
        assert_eq!(c.advance(at(SUN + 1, 2, 0), [("q", &s)]).len(), 1);
    }

    #[test]
    fn a_dst_gap_still_fires_the_skipped_minute() {
        // Clocks jump from 01:59 to 03:00; the start is at 02:30.
        let s = sched(Some(("02:30", &[])), None);
        let mut c = Crossings::default();
        c.advance(at(SUN, 1, 59), [("q", &s)]);
        assert_eq!(
            c.advance(at(SUN, 3, 0), [("q", &s)]),
            vec![("q".into(), Edge::Start)]
        );
    }

    #[test]
    fn a_repeated_hour_fires_once() {
        // Clocks go back from 01:59 to 01:00; the stop is at 01:30.
        let s = sched(None, Some(("01:30", &[])));
        let mut c = Crossings::default();
        c.advance(at(SUN, 1, 0), [("q", &s)]);
        assert_eq!(c.advance(at(SUN, 1, 30), [("q", &s)]).len(), 1);
        c.advance(at(SUN, 1, 59), [("q", &s)]);
        assert!(c.advance(at(SUN, 1, 0), [("q", &s)]).is_empty(), "back");
        assert!(
            c.advance(at(SUN, 1, 45), [("q", &s)]).is_empty(),
            "01:30 again"
        );
        assert_eq!(
            c.advance(at(SUN + 1, 1, 30), [("q", &s)]).len(),
            1,
            "tomorrow"
        );
    }

    #[test]
    fn after_sleeping_through_both_edges_only_the_latest_applies() {
        let s = sched(Some(("01:00", &[])), Some(("07:00", &[])));
        let mut c = Crossings::default();
        c.advance(at(SUN, 0, 30), [("q", &s)]);
        assert_eq!(
            c.advance(at(SUN, 8, 0), [("q", &s)]),
            vec![("q".into(), Edge::Stop)]
        );
        // Asleep for two days, waking at 03:00: the last event was a start.
        assert_eq!(
            c.advance(at(SUN + 2, 3, 0), [("q", &s)]),
            vec![("q".into(), Edge::Start)]
        );
        // Asleep for a month: still only the latest event.
        assert_eq!(
            c.advance(at(SUN + 40, 9, 0), [("q", &s)]),
            vec![("q".into(), Edge::Stop)]
        );
    }

    #[test]
    fn a_time_zone_change_west_does_not_repeat_the_evening() {
        let s = sched(Some(("20:00", &[])), None);
        let mut c = Crossings::default();
        c.advance(at(SUN, 19, 59), [("q", &s)]);
        assert_eq!(c.advance(at(SUN, 20, 0), [("q", &s)]).len(), 1);
        // Five hours west: 21:00 becomes 16:00.
        c.advance(at(SUN, 21, 0), [("q", &s)]);
        c.advance(at(SUN, 16, 0), [("q", &s)]);
        assert!(c.advance(at(SUN, 20, 30), [("q", &s)]).is_empty());
    }
}
