//! Queues and the scheduler (PROTOCOL.md §3, "Queues and the scheduler").
//!
//! Every download is in one queue: the main queue (named [`MAIN_QUEUE`],
//! the default) or a named queue. A queue starts up to `max_active` of its
//! downloads at once, oldest first, and may carry a [`Schedule`]: when to
//! start, when to stop, and what to do when everything in it has finished.
//! Clock times are the daemon machine's local time.

use serde::{Deserialize, Serialize};

/// The main queue's name. `download.add` without `queue` (or with
/// `"main"`) puts a download here; it cannot be renamed or removed, and its
/// `max_active` is `Settings::max_active_downloads`.
pub const MAIN_QUEUE: &str = "main";

/// Longest queue name, in characters.
pub const QUEUE_NAME_MAX: usize = 64;

/// A queue as `queue.list` and `event.queues` report it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Queue {
    pub name: String,
    /// How many of its downloads run at once (1–64). Named queues default to 1.
    pub max_active: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<Schedule>,
    /// False after `queue.stop`, until `queue.start` or the next scheduled
    /// start: its downloads wait.
    pub running: bool,
}

/// What `queue.create` and `queue.update` set. `max_active` and `schedule`
/// replace the queue's values; an absent `schedule` removes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueConfig {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_active: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<Schedule>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Schedule {
    /// Start the queue at this time (on these days).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<ClockTime>,
    /// Stop the queue (pause what it is running) at this time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<ClockTime>,
    /// What to do once every download in the queue has finished.
    #[serde(default)]
    pub after_done: AfterDone,
}

/// A time of day, `"HH:MM"` (24-hour), on some days of the week.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClockTime {
    pub at: String,
    /// 0 = Sunday … 6 = Saturday; empty means every day.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub days: Vec<u8>,
}

/// Run once a queue has nothing left to do. The daemon announces it with
/// `event.queue.finished` and waits [`AFTER_DONE_GRACE_SECS`] so a client
/// can cancel it (`queue.cancelAction`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AfterDone {
    #[default]
    None,
    /// Put the computer to sleep.
    Sleep,
    /// Shut the computer down.
    Shutdown,
}

/// Seconds between `event.queue.finished` and the action.
pub const AFTER_DONE_GRACE_SECS: u64 = 60;

/// A speed limit that applies at certain times (`Settings::speed_schedule`).
/// While a window is open its `limit_bps` replaces `Settings::speed_limit_bps`;
/// where windows overlap, the lowest limit wins. `from` after `to` spans
/// midnight.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeedWindow {
    /// 0 = Sunday … 6 = Saturday (the day the window opens); empty means every day.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub days: Vec<u8>,
    /// `"HH:MM"`.
    pub from: String,
    /// `"HH:MM"`.
    pub to: String,
    pub limit_bps: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueNameParams {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueRenameParams {
    pub name: String,
    pub new_name: String,
}

/// `download.setQueue`: move downloads to `queue` (absent = the main queue).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetQueueParams {
    pub ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue: Option<String>,
}

/// `event.queue.finished`: a queue has finished and its `after_done` action
/// runs at `at_ms` unless a client calls `queue.cancelAction` first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueFinishedEvent {
    pub name: String,
    pub action: AfterDone,
    pub at_ms: i64,
}

/// Whether `s` is a valid `"HH:MM"`.
pub fn is_clock_time(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 5
        || b[2] != b':'
        || !b
            .iter()
            .enumerate()
            .all(|(i, c)| i == 2 || c.is_ascii_digit())
    {
        return false;
    }
    let h = u32::from(b[0] - b'0') * 10 + u32::from(b[1] - b'0');
    let m = u32::from(b[3] - b'0') * 10 + u32::from(b[4] - b'0');
    h < 24 && m < 60
}

/// Whether `name` can name a queue: 1–64 characters, not only whitespace,
/// no control characters. ([`MAIN_QUEUE`] names the main queue.)
pub fn is_queue_name(name: &str) -> bool {
    let n = name.chars().count();
    (1..=QUEUE_NAME_MAX).contains(&n)
        && !name.trim().is_empty()
        && !name.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_times() {
        for ok in ["00:00", "09:30", "23:59"] {
            assert!(is_clock_time(ok), "{ok}");
        }
        for bad in ["24:00", "9:30", "09:60", "0930", "09:3a", "", "09:30:00"] {
            assert!(!is_clock_time(bad), "{bad}");
        }
    }

    #[test]
    fn queue_names() {
        assert!(is_queue_name("Night"));
        assert!(is_queue_name("রাতের ডাউনলোড"));
        assert!(!is_queue_name(""));
        assert!(!is_queue_name("   "));
        assert!(!is_queue_name("a\nb"));
        assert!(!is_queue_name(&"x".repeat(65)));
    }

    #[test]
    fn wire_format() {
        let q = Queue {
            name: "Night".into(),
            max_active: 2,
            schedule: Some(Schedule {
                start: Some(ClockTime {
                    at: "01:00".into(),
                    days: vec![],
                }),
                stop: Some(ClockTime {
                    at: "07:00".into(),
                    days: vec![1, 2],
                }),
                after_done: AfterDone::Sleep,
            }),
            running: true,
        };
        let v = serde_json::to_value(&q).unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "name": "Night", "maxActive": 2, "running": true,
                "schedule": {
                    "start": { "at": "01:00" },
                    "stop": { "at": "07:00", "days": [1, 2] },
                    "afterDone": "sleep"
                }
            })
        );
        assert_eq!(serde_json::from_value::<Queue>(v).unwrap(), q);
        let s: Schedule = serde_json::from_str("{}").unwrap();
        assert_eq!(s.after_done, AfterDone::None);
    }
}
