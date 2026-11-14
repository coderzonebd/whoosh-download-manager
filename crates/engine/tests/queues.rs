//! Queues and the scheduler (PROTOCOL.md "Queues and the scheduler"):
//! queue management, per-queue `maxActive`, stop/start, persistence,
//! scheduled starts and stops, speed windows and after-done actions.
//!
//! Time is tokio's paused clock; the wall clock is a [`FakeClock`] that runs
//! with it (and can jump), and after-done actions go to a [`FakePower`].

mod common;

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use common::*;
use futures::stream::BoxStream;
use futures::{FutureExt, StreamExt};
use tokio::time::Instant;
use whoosh_core::StateStore;
use whoosh_engine::{Clock, Engine, EngineConfig, LocalTime, QUEUES_KEY};
use whoosh_proto::queue::AFTER_DONE_GRACE_SECS;
use whoosh_proto::service::{Notification, Service, ServiceError};
use whoosh_proto::{
    AfterDone, ClockTime, DownloadState, IdsParams, ListParams, MAIN_QUEUE, Queue, QueueConfig,
    QueueNameParams, QueueRenameParams, Schedule, SetQueueParams, SpeedWindow, error_code,
};

const LONG: Duration = Duration::from_secs(3600);
const INVALID: i32 = ServiceError::INVALID_PARAMS;

/// Sunday 2026-11-15 at `h:m`, local time.
fn sunday(h: u32, m: u32) -> LocalTime {
    LocalTime::from_civil(2026, 11, 15, h, m)
}

struct Rig {
    lab: Lab,
    clock: Arc<FakeClock>,
    power: Arc<FakePower>,
    sources: std::sync::Mutex<std::collections::HashMap<String, Arc<FaultSource>>>,
}

impl Rig {
    async fn at(t: LocalTime) -> Self {
        let clock = FakeClock::at(t);
        let power = Arc::new(FakePower::default());
        let lab = Lab::new(cfg(&clock, &power)).await;
        Self {
            lab,
            clock,
            power,
            sources: Default::default(),
        }
    }

    fn source(&self, url: &str) -> Arc<FaultSource> {
        lock(&self.sources)[url].clone()
    }

    fn engine(&self) -> &Engine {
        &self.lab.engine
    }

    async fn queue(&self, name: &str) -> Queue {
        self.engine()
            .queues()
            .await
            .unwrap()
            .into_iter()
            .find(|q| q.name == name)
            .unwrap_or_else(|| panic!("no queue {name}"))
    }

    /// Serve `url` slowly and add it to `queue`, on one connection.
    async fn add_slow(&self, url: &str, queue: &str, size: u64, bps: u64) -> String {
        let src = self.lab.serve(url, slow(content(size, size ^ bps), bps));
        lock(&self.sources).insert(url.into(), src);
        let mut p = add_params(url);
        p.queue = Some(queue.into());
        p.connections = Some(1);
        self.engine().add(p).await.unwrap().id
    }

    async fn state(&self, id: &str) -> DownloadState {
        detail(self.engine(), id).await.summary.state
    }
}

fn cfg(clock: &Arc<FakeClock>, power: &Arc<FakePower>) -> EngineConfig {
    let mut c = config();
    c.clock = clock.clone();
    c.power = power.clone();
    c
}

fn slow(data: Bytes, bps: u64) -> Arc<FaultSource> {
    Arc::new(FaultSource::new(data).cfg(|c| {
        c.per_conn_bps = Some(bps);
        c.chunk = 16 * KIB as usize;
    }))
}

fn qc(name: &str, max_active: Option<u32>, schedule: Option<Schedule>) -> QueueConfig {
    QueueConfig {
        name: name.into(),
        max_active,
        schedule,
    }
}

fn at(t: &str) -> Option<ClockTime> {
    Some(ClockTime {
        at: t.into(),
        days: vec![],
    })
}

fn sched(start: Option<&str>, stop: Option<&str>, after_done: AfterDone) -> Schedule {
    Schedule {
        start: start.and_then(at),
        stop: stop.and_then(at),
        after_done,
    }
}

fn name(n: &str) -> QueueNameParams {
    QueueNameParams { name: n.into() }
}

fn url(i: usize) -> String {
    format!("https://lab.test/q/file{i}.bin")
}

fn is_running(s: DownloadState) -> bool {
    matches!(
        s,
        DownloadState::Probing | DownloadState::Downloading | DownloadState::Verifying
    )
}

/// Every notification available right now.
fn drain(events: &mut BoxStream<'static, Notification>) -> Vec<Notification> {
    let mut out = Vec::new();
    while let Some(Some(n)) = events.next().now_or_never() {
        out.push(n);
    }
    out
}

/// The `event.queues` payloads available right now.
fn queue_events(events: &mut BoxStream<'static, Notification>) -> Vec<Vec<Queue>> {
    drain(events)
        .into_iter()
        .filter_map(|n| match n {
            Notification::Queues(q) => Some(q),
            _ => None,
        })
        .collect()
}

/// Waits (virtual time) for the next `event.queue.finished`.
async fn next_finished(
    events: &mut BoxStream<'static, Notification>,
) -> whoosh_proto::QueueFinishedEvent {
    tokio::time::timeout(LONG, async {
        loop {
            if let Notification::QueueFinished(e) = events.next().await.expect("stream ended") {
                return e;
            }
        }
    })
    .await
    .expect("no event.queue.finished")
}

// ------------------------------------------------------------ management

#[tokio::test(start_paused = true)]
async fn queues_are_created_updated_renamed_and_removed() {
    let rig = Rig::at(sunday(12, 0)).await;
    let e = rig.engine();
    assert!(e.capabilities().contains(&"queues".to_string()));
    let mut events = e.subscribe();

    // Only the main queue at first; it follows maxActiveDownloads.
    let main = Queue {
        name: MAIN_QUEUE.into(),
        max_active: 3,
        schedule: None,
        running: true,
    };
    assert_eq!(e.queues().await.unwrap(), vec![main.clone()]);

    // Create: maxActive defaults to 1; a queue with a start time waits.
    let night = e.create_queue(qc("Night", None, None)).await.unwrap();
    assert_eq!((night.max_active, night.running), (1, true));
    let timed = e
        .create_queue(qc(
            "Timed",
            Some(4),
            Some(sched(Some("01:00"), None, AfterDone::None)),
        ))
        .await
        .unwrap();
    assert_eq!((timed.max_active, timed.running), (4, false));
    let evs = queue_events(&mut events);
    assert_eq!(evs.len(), 2, "one event.queues per change");
    assert_eq!(evs[1], vec![main.clone(), night.clone(), timed.clone()]);
    assert_eq!(
        e.queues()
            .await
            .unwrap()
            .iter()
            .map(|q| q.name.as_str())
            .collect::<Vec<_>>(),
        ["main", "Night", "Timed"],
        "main first, then by name"
    );

    // Bad input.
    for bad in [
        qc("Night", None, None),
        qc("main", None, None),
        qc("", None, None),
        qc("  ", None, None),
        qc("a\u{7}b", None, None),
        qc(&"x".repeat(65), None, None),
        qc("Zero", Some(0), None),
        qc("Many", Some(65), None),
        qc(
            "BadTime",
            None,
            Some(sched(Some("25:00"), None, AfterDone::None)),
        ),
        qc(
            "BadDay",
            None,
            Some(Schedule {
                start: Some(ClockTime {
                    at: "01:00".into(),
                    days: vec![7],
                }),
                stop: None,
                after_done: AfterDone::None,
            }),
        ),
    ] {
        let err = e.create_queue(bad.clone()).await.unwrap_err();
        assert_eq!(err.code, INVALID, "{bad:?}: {err}");
    }
    assert!(
        queue_events(&mut events).is_empty(),
        "no event for refusals"
    );

    // Update replaces maxActive and the schedule; an absent schedule
    // removes it, an absent maxActive keeps it.
    let s = sched(Some("02:00"), Some("06:00"), AfterDone::Sleep);
    let q = e
        .update_queue(qc("Night", Some(2), Some(s.clone())))
        .await
        .unwrap();
    assert_eq!((q.max_active, q.schedule.as_ref()), (2, Some(&s)));
    let q = e.update_queue(qc("Night", None, None)).await.unwrap();
    assert_eq!((q.max_active, q.schedule), (2, None));
    assert_eq!(queue_events(&mut events).len(), 2);
    assert_eq!(
        e.update_queue(qc("Nope", None, None))
            .await
            .unwrap_err()
            .code,
        error_code::NOT_FOUND
    );

    // The main queue: maxActive is Settings.maxActiveDownloads; no schedule.
    let q = e.update_queue(qc(MAIN_QUEUE, Some(5), None)).await.unwrap();
    assert_eq!(q.max_active, 5);
    assert_eq!(e.settings().await.unwrap().max_active_downloads, 5);
    let evs = drain(&mut events);
    assert!(
        evs.iter()
            .any(|n| matches!(n, Notification::Settings(s) if s.max_active_downloads == 5))
    );
    assert!(
        evs.iter()
            .any(|n| matches!(n, Notification::Queues(q) if q[0].max_active == 5))
    );
    let err = e
        .update_queue(qc(
            MAIN_QUEUE,
            None,
            Some(sched(Some("01:00"), None, AfterDone::None)),
        ))
        .await
        .unwrap_err();
    assert_eq!(err.code, INVALID);
    assert_eq!(
        e.update_queue(qc(MAIN_QUEUE, Some(0), None))
            .await
            .unwrap_err()
            .code,
        INVALID
    );
    // settings.set changing maxActiveDownloads also announces the queues.
    let mut st = e.settings().await.unwrap();
    st.max_active_downloads = 3;
    e.set_settings(st).await.unwrap();
    assert!(queue_events(&mut events).last().unwrap()[0].max_active == 3);

    // Downloads follow a rename, and go to the main queue on removal.
    for i in 0..3 {
        rig.lab
            .serve(&url(i), Arc::new(FaultSource::new(content(KIB, i as u64))));
    }
    let mut p = add_params(&url(0));
    p.start = false;
    p.queue = Some("Night".into());
    let id = e.add(p).await.unwrap().id;
    drain(&mut events);
    let q = e
        .rename_queue(QueueRenameParams {
            name: "Night".into(),
            new_name: "Late".into(),
        })
        .await
        .unwrap();
    assert_eq!((q.name.as_str(), q.max_active), ("Late", 2));
    assert_eq!(
        rig.lab.engine.record(&id).unwrap().queue.as_deref(),
        Some("Late")
    );
    let evs = drain(&mut events);
    assert!(evs.iter().any(
        |n| matches!(n, Notification::DownloadUpdated(s) if s.id == id && s.queue.as_deref() == Some("Late"))
    ));
    assert!(evs.iter().any(|n| matches!(n, Notification::Queues(_))));
    for (from, to, code) in [
        ("Late", "Timed", INVALID),
        ("Late", "main", INVALID),
        ("Late", "", INVALID),
        ("main", "Other", INVALID),
        ("Nope", "Other", error_code::NOT_FOUND),
    ] {
        let err = e
            .rename_queue(QueueRenameParams {
                name: from.into(),
                new_name: to.into(),
            })
            .await
            .unwrap_err();
        assert_eq!(err.code, code, "{from} -> {to}");
    }
    e.remove_queue(name("Late")).await.unwrap();
    assert_eq!(rig.lab.engine.record(&id).unwrap().queue, None);
    let evs = drain(&mut events);
    assert!(
        evs.iter().any(
            |n| matches!(n, Notification::DownloadUpdated(s) if s.id == id && s.queue.is_none())
        )
    );
    assert_eq!(
        e.queues().await.unwrap().len(),
        2,
        "main and Timed are left"
    );
    assert_eq!(
        e.remove_queue(name("main")).await.unwrap_err().code,
        INVALID
    );
    assert_eq!(
        e.remove_queue(name("Late")).await.unwrap_err().code,
        error_code::NOT_FOUND
    );

    // download.setQueue: to a queue, back to main; unknown names refused.
    let set = |queue: Option<&str>, ids: Vec<String>| SetQueueParams {
        ids,
        queue: queue.map(str::to_string),
    };
    e.set_queue(set(Some("Timed"), vec![id.clone()]))
        .await
        .unwrap();
    assert_eq!(
        rig.lab.engine.record(&id).unwrap().queue.as_deref(),
        Some("Timed")
    );
    e.set_queue(set(Some(MAIN_QUEUE), vec![id.clone()]))
        .await
        .unwrap();
    assert_eq!(rig.lab.engine.record(&id).unwrap().queue, None);
    e.set_queue(set(Some("Timed"), vec![id.clone()]))
        .await
        .unwrap();
    e.set_queue(set(None, vec![id.clone()])).await.unwrap();
    assert_eq!(rig.lab.engine.record(&id).unwrap().queue, None);
    assert_eq!(
        e.set_queue(set(Some("Nope"), vec![id.clone()]))
            .await
            .unwrap_err()
            .code,
        error_code::NOT_FOUND
    );
    assert_eq!(
        e.set_queue(set(None, vec!["dl_missing".into()]))
            .await
            .unwrap_err()
            .code,
        error_code::NOT_FOUND
    );

    // download.add to an unknown queue creates it (the CLI's --queue).
    let mut p = add_params(&url(1));
    p.start = false;
    p.queue = Some("Fresh".into());
    e.add(p).await.unwrap();
    assert_eq!(rig.queue("Fresh").await.max_active, 1);
    let mut p = add_params(&url(2));
    p.queue = Some("bad\nname".into());
    assert_eq!(e.add(p).await.unwrap_err().code, INVALID);

    // Start and stop: not for the main queue; unknown queues not found.
    e.start_queue(name(MAIN_QUEUE)).await.unwrap();
    assert_eq!(
        e.stop_queue(name(MAIN_QUEUE)).await.unwrap_err().code,
        INVALID
    );
    for f in [
        e.start_queue(name("Nope")),
        e.stop_queue(name("Nope")),
        e.cancel_queue_action(name("Nope")),
    ] {
        assert_eq!(f.await.unwrap_err().code, error_code::NOT_FOUND);
    }
    e.cancel_queue_action(name("Timed")).await.unwrap();
    e.cancel_queue_action(name(MAIN_QUEUE)).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn each_named_queue_runs_its_own_max_active() {
    let rig = Rig::at(sunday(12, 0)).await;
    let e = rig.engine();
    let mut s = e.settings().await.unwrap();
    s.max_active_downloads = 1;
    e.set_settings(s).await.unwrap();
    e.create_queue(qc("A", Some(2), None)).await.unwrap();
    e.create_queue(qc("B", None, None)).await.unwrap();
    let mut n = 0;
    for (queue, count) in [("A", 3), ("B", 2), (MAIN_QUEUE, 2)] {
        for _ in 0..count {
            rig.add_slow(&url(n), queue, 2 * MIB, MIB).await;
            n += 1;
        }
    }
    let t0 = Instant::now();
    let mut peak = std::collections::HashMap::<String, usize>::new();
    loop {
        let all = e.list(ListParams::default()).await.unwrap();
        let mut now = std::collections::HashMap::<String, usize>::new();
        for d in all.iter().filter(|d| is_running(d.state)) {
            *now.entry(d.queue.clone().unwrap_or_else(|| MAIN_QUEUE.into()))
                .or_default() += 1;
        }
        for (q, c) in now {
            let p = peak.entry(q).or_default();
            *p = (*p).max(c);
        }
        if all.iter().all(|d| d.state == DownloadState::Completed) {
            break;
        }
        assert!(t0.elapsed() < LONG, "{all:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(peak["A"], 2, "{peak:?}");
    assert_eq!(peak["B"], 1, "{peak:?}");
    assert_eq!(peak[MAIN_QUEUE], 1, "{peak:?}");
    // The queues ran side by side: 7 downloads of 2 s each, three lanes.
    assert!(t0.elapsed() < Duration::from_secs(10), "{:?}", t0.elapsed());
}

#[tokio::test(start_paused = true)]
async fn stop_holds_what_runs_and_start_carries_on() {
    let rig = Rig::at(sunday(12, 0)).await;
    let e = rig.engine();
    e.create_queue(qc("Q", None, None)).await.unwrap();
    let ids = [
        rig.add_slow(&url(0), "Q", 4 * MIB, MIB).await,
        rig.add_slow(&url(1), "Q", 4 * MIB, MIB).await,
    ];
    wait_for(e, &ids[0], &[DownloadState::Downloading], LONG).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let mut events = e.subscribe();

    // stop answers once the running download has stopped.
    e.stop_queue(name("Q")).await.unwrap();
    assert!(!rig.queue("Q").await.running);
    assert!(
        queue_events(&mut events)
            .iter()
            .any(|q| q.iter().any(|q| q.name == "Q" && !q.running))
    );
    for id in &ids {
        assert_eq!(
            rig.state(id).await,
            DownloadState::Queued,
            "held, not paused"
        );
    }
    let held = detail(e, &ids[0]).await.summary.downloaded_bytes;
    assert!(held > 0 && held < 4 * MIB, "{held}");
    // A resume does not start a held download, and nothing runs meanwhile.
    e.resume(IdsParams { ids: ids.to_vec() }).await.unwrap();
    tokio::time::sleep(Duration::from_secs(30)).await;
    for id in &ids {
        assert_eq!(rig.state(id).await, DownloadState::Queued);
    }
    assert_eq!(detail(e, &ids[0]).await.summary.downloaded_bytes, held);
    assert_eq!(e.stats().await.unwrap().active, 0);
    // A download moved into a stopped queue is held too.
    let other = rig.add_slow(&url(2), MAIN_QUEUE, 4 * MIB, MIB).await;
    wait_for(e, &other, &[DownloadState::Downloading], LONG).await;
    e.set_queue(SetQueueParams {
        ids: vec![other.clone()],
        queue: Some("Q".into()),
    })
    .await
    .unwrap();
    wait_for(e, &other, &[DownloadState::Queued], LONG).await;

    // start: the first one carries on from where it was, then the rest.
    e.start_queue(name("Q")).await.unwrap();
    assert!(rig.queue("Q").await.running);
    for id in &ids {
        finish(e, id, LONG).await;
    }
    finish(e, &other, LONG).await;
    assert!(
        rig.source(&url(0)).served() < 4 * MIB + MIB,
        "resumed, not restarted"
    );
}

#[tokio::test(start_paused = true)]
async fn queues_persist_across_a_restart() {
    let rig = Rig::at(sunday(12, 0)).await;
    let e = rig.engine();
    e.create_queue(qc(
        "Night",
        Some(3),
        Some(sched(Some("01:00"), Some("06:00"), AfterDone::Shutdown)),
    ))
    .await
    .unwrap();
    e.create_queue(qc("Day", Some(2), None)).await.unwrap();
    e.stop_queue(name("Day")).await.unwrap();
    for i in 0..2 {
        rig.lab
            .serve(&url(i), Arc::new(FaultSource::new(content(KIB, i as u64))));
    }
    let mut p = add_params(&url(0));
    p.start = false;
    p.queue = Some("Day".into());
    let id = e.add(p).await.unwrap().id;
    // A download whose queue was never stored (put there before queues
    // were): its queue is recreated with the defaults.
    let mut p = add_params(&url(1));
    p.start = false;
    p.queue = Some("Legacy".into());
    let legacy = e.add(p).await.unwrap().id;
    let before = e.queues().await.unwrap();
    e.shutdown().await;
    let stored: Vec<Queue> =
        serde_json::from_str(&rig.lab.state.get_value(QUEUES_KEY).await.unwrap().unwrap()).unwrap();
    let without_legacy: Vec<&Queue> = stored.iter().filter(|q| q.name != "Legacy").collect();
    rig.lab
        .state
        .set_value(QUEUES_KEY, &serde_json::to_string(&without_legacy).unwrap())
        .await
        .unwrap();

    let again = Engine::start(rig.lab.deps(), cfg(&rig.clock, &rig.power))
        .await
        .unwrap();
    assert_eq!(again.queues().await.unwrap(), before);
    assert!(!before.iter().find(|q| q.name == "Day").unwrap().running);
    assert!(!before.iter().find(|q| q.name == "Night").unwrap().running);
    assert_eq!(again.record(&id).unwrap().queue.as_deref(), Some("Day"));
    assert_eq!(
        again.record(&legacy).unwrap().queue.as_deref(),
        Some("Legacy")
    );
    // The recreated queue is stored again.
    let stored = rig.lab.state.get_value(QUEUES_KEY).await.unwrap().unwrap();
    assert!(stored.contains("Legacy"), "{stored}");
    again.shutdown().await;
}

// -------------------------------------------------------------- scheduler

#[tokio::test(start_paused = true)]
async fn a_queue_starts_and_stops_at_its_times() {
    let rig = Rig::at(sunday(0, 58)).await;
    let e = rig.engine();
    let q = e
        .create_queue(qc(
            "Night",
            None,
            Some(sched(Some("01:00"), Some("01:05"), AfterDone::None)),
        ))
        .await
        .unwrap();
    assert!(!q.running, "waits for its start");
    // 32 MiB at 64 KiB/s: longer than the five-minute window.
    let id = rig.add_slow(&url(0), "Night", 32 * MIB, 64 * KIB).await;
    let mut events = e.subscribe();
    tokio::time::sleep(Duration::from_secs(100)).await; // 00:59:40
    assert_eq!(rig.state(&id).await, DownloadState::Queued);
    assert!(queue_events(&mut events).is_empty());

    // 01:00: started (within a tick of the minute).
    wait_for(
        e,
        &id,
        &[DownloadState::Downloading],
        Duration::from_secs(25),
    )
    .await;
    assert_eq!(rig.clock.local(), sunday(1, 0));
    assert!(rig.queue("Night").await.running);
    assert!(
        queue_events(&mut events)
            .iter()
            .any(|q| q.iter().any(|q| q.name == "Night" && q.running))
    );

    // 01:05: held again, part done.
    wait_for(e, &id, &[DownloadState::Queued], Duration::from_secs(330)).await;
    assert_eq!(rig.clock.local(), sunday(1, 5));
    assert!(!rig.queue("Night").await.running);
    let got = detail(e, &id).await.summary.downloaded_bytes;
    assert!(got > 8 * MIB && got < 32 * MIB, "{got}");

    // The machine sleeps until 00:59 the next day: nothing was crossed.
    rig.clock.jump(((23 * 60 + 54) * 60) as i64 * 1000);
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(
        rig.clock.local(),
        LocalTime::from_civil(2026, 11, 16, 0, 59)
    );
    assert_eq!(rig.state(&id).await, DownloadState::Queued);
    // Monday 01:00: runs again, and carries on.
    wait_for(
        e,
        &id,
        &[DownloadState::Downloading],
        Duration::from_secs(70),
    )
    .await;
    assert!(detail(e, &id).await.summary.downloaded_bytes >= got);
}

#[tokio::test(start_paused = true)]
async fn a_start_slept_through_or_skipped_by_dst_still_happens() {
    let rig = Rig::at(sunday(1, 50)).await;
    let e = rig.engine();
    e.create_queue(qc(
        "Gap",
        None,
        Some(sched(Some("02:30"), None, AfterDone::None)),
    ))
    .await
    .unwrap();
    e.create_queue(qc(
        "Slept",
        None,
        Some(sched(Some("04:00"), Some("05:00"), AfterDone::None)),
    ))
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(599_500)).await; // 01:59:59.5
    assert!(!rig.queue("Gap").await.running);
    // DST: 02:00 becomes 03:00, so 02:30 never shows.
    rig.clock.shift_zone(60);
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(rig.clock.local(), sunday(3, 0));
    assert!(rig.queue("Gap").await.running, "started at 03:00");
    assert!(!rig.queue("Slept").await.running);
    // Asleep from 03:00 to 06:00: Slept's start and stop both passed; it
    // ends up stopped and never ran a download meanwhile.
    let id = rig.add_slow(&url(0), "Slept", MIB, MIB).await;
    rig.clock.jump(180 * 60 * 1000);
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(rig.clock.local(), sunday(6, 0));
    assert!(!rig.queue("Slept").await.running);
    assert_eq!(rig.state(&id).await, DownloadState::Queued);
    assert_eq!(rig.source(&url(0)).requests(), 0);
    // The clock goes back 90 minutes, to 04:30: the 05:00 stop already
    // happened today and does not fire again, so a manual start holds.
    e.start_queue(name("Slept")).await.unwrap();
    rig.clock.shift_zone(-90);
    tokio::time::sleep(Duration::from_secs(32 * 60)).await;
    assert_eq!(rig.clock.local(), sunday(5, 2));
    assert!(rig.queue("Slept").await.running);
    finish(e, &id, LONG).await;
}

#[tokio::test(start_paused = true)]
async fn speed_windows_replace_the_limit_while_open() {
    let rig = Rig::at(sunday(0, 59)).await;
    let e = rig.engine();
    let mut s = e.settings().await.unwrap();
    s.speed_limit_bps = Some(MIB);
    s.speed_schedule = vec![
        SpeedWindow {
            days: vec![],
            from: "01:00".into(),
            to: "02:00".into(),
            limit_bps: 4 * MIB,
        },
        SpeedWindow {
            days: vec![],
            from: "01:30".into(),
            to: "01:45".into(),
            limit_bps: 2 * MIB,
        },
    ];
    e.set_settings(s.clone()).await.unwrap();
    // 00:59: the base limit, 1 MiB/s → about 8 s for 8 MiB.
    let t = rig.took(1).await;
    assert!((7.2..=9.2).contains(&t), "base: {t}");
    // 01:00–02:00 window (higher than the base, still replaces it).
    tokio::time::sleep(Duration::from_secs(62)).await;
    let t = rig.took(2).await;
    assert!((1.6..=2.6).contains(&t), "window: {t}");
    // 01:30–01:45 overlaps it: the lowest wins.
    tokio::time::sleep(Duration::from_secs(30 * 60)).await;
    assert!(rig.clock.local() >= sunday(1, 30));
    let t = rig.took(3).await;
    assert!((3.4..=4.6).contains(&t), "overlap: {t}");
    // After 02:00, the base again.
    tokio::time::sleep(Duration::from_secs(40 * 60)).await;
    let t = rig.took(4).await;
    assert!((7.2..=9.2).contains(&t), "after: {t}");

    // Bad windows are refused.
    for bad in [
        SpeedWindow {
            days: vec![],
            from: "1:00".into(),
            to: "02:00".into(),
            limit_bps: 1,
        },
        SpeedWindow {
            days: vec![],
            from: "01:00".into(),
            to: "02:00".into(),
            limit_bps: 0,
        },
        SpeedWindow {
            days: vec![8],
            from: "01:00".into(),
            to: "02:00".into(),
            limit_bps: 1,
        },
    ] {
        let mut s = s.clone();
        s.speed_schedule = vec![bad];
        assert_eq!(e.set_settings(s).await.unwrap_err().code, INVALID);
    }
}

#[tokio::test(start_paused = true)]
async fn a_speed_window_over_midnight_belongs_to_the_day_it_opens() {
    // Saturday 2026-11-14, 22:59.
    let rig = Rig::at(LocalTime::from_civil(2026, 11, 14, 22, 59)).await;
    let e = rig.engine();
    let mut s = e.settings().await.unwrap();
    s.speed_schedule = vec![SpeedWindow {
        days: vec![6],
        from: "23:00".into(),
        to: "01:00".into(),
        limit_bps: MIB,
    }];
    e.set_settings(s).await.unwrap();
    assert!(rig.took(1).await < 1.0, "Saturday 22:59: unlimited");
    tokio::time::sleep(Duration::from_secs(62)).await;
    let t = rig.took(2).await;
    assert!((7.2..=9.2).contains(&t), "Saturday 23:00: {t}");
    // Sunday 00:30: still Saturday's window.
    tokio::time::sleep(Duration::from_secs(90 * 60)).await;
    assert_eq!(rig.clock.local().weekday(), 0);
    let t = rig.took(3).await;
    assert!((7.2..=9.2).contains(&t), "Sunday 00:30: {t}");
    // Sunday 01:00 closes it, and Sunday 23:00 opens none.
    tokio::time::sleep(Duration::from_secs(30 * 60)).await;
    assert!(rig.clock.local() >= sunday(1, 0));
    assert!(rig.took(4).await < 1.0, "Sunday 01:00");
    rig.clock.jump(22 * 3600 * 1000);
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(rig.clock.local() >= sunday(23, 0));
    assert!(rig.took(5).await < 1.0, "Sunday 23:00");
}

impl Rig {
    /// Seconds an unpaced 8 MiB download takes now (so: the speed limit).
    async fn took(&self, n: usize) -> f64 {
        let u = url(100 + n);
        self.lab
            .serve(&u, Arc::new(FaultSource::new(content(8 * MIB, n as u64))));
        let mut p = add_params(&u);
        p.connections = Some(1);
        let id = self.engine().add(p).await.unwrap().id;
        finish(self.engine(), &id, LONG).await.1.as_secs_f64()
    }
}

// ------------------------------------------------------------- after done

#[tokio::test(start_paused = true)]
async fn the_after_done_action_runs_after_the_grace_period() {
    let rig = Rig::at(sunday(10, 0)).await;
    let e = rig.engine();
    e.create_queue(qc("Q", None, Some(sched(None, None, AfterDone::Shutdown))))
        .await
        .unwrap();
    let mut events = e.subscribe();
    let a = rig.add_slow(&url(0), "Q", MIB, MIB).await;
    let b = rig.add_slow(&url(1), "Q", MIB, MIB).await;
    let ev = next_finished(&mut events).await;
    let announced = Instant::now();
    assert_eq!(ev.name, "Q");
    assert_eq!(ev.action, AfterDone::Shutdown);
    assert!(
        (ev.at_ms - (rig.clock.now_ms() + 60_000)).abs() <= 1000,
        "{ev:?}"
    );
    for id in [&a, &b] {
        assert_eq!(
            rig.state(id).await,
            DownloadState::Completed,
            "only once all are done"
        );
    }
    tokio::time::sleep(Duration::from_secs(AFTER_DONE_GRACE_SECS - 1)).await;
    assert!(rig.power.actions().is_empty());
    tokio::time::sleep(Duration::from_secs(2)).await;
    let ran = rig.power.ran.lock().unwrap().clone();
    assert_eq!(ran.len(), 1);
    assert_eq!(ran[0].0, AfterDone::Shutdown);
    let after = (ran[0].1 - announced).as_secs_f64();
    assert!((59.9..=60.1).contains(&after), "{after}");
    // Once only.
    tokio::time::sleep(Duration::from_secs(300)).await;
    assert_eq!(rig.power.actions().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_cancelled_after_done_action_never_runs() {
    let rig = Rig::at(sunday(10, 0)).await;
    let e = rig.engine();
    e.create_queue(qc("Q", None, Some(sched(None, None, AfterDone::Sleep))))
        .await
        .unwrap();
    let mut events = e.subscribe();
    rig.add_slow(&url(0), "Q", MIB, MIB).await;
    let ev = next_finished(&mut events).await;
    assert_eq!(ev.action, AfterDone::Sleep);
    tokio::time::sleep(Duration::from_secs(30)).await;
    e.cancel_queue_action(name("Q")).await.unwrap();
    tokio::time::sleep(Duration::from_secs(300)).await;
    assert!(rig.power.actions().is_empty());

    // The queue finishing again announces a new action, which runs.
    rig.add_slow(&url(1), "Q", MIB, MIB).await;
    next_finished(&mut events).await;
    tokio::time::sleep(Duration::from_secs(AFTER_DONE_GRACE_SECS + 1)).await;
    assert_eq!(rig.power.actions(), vec![AfterDone::Sleep]);
}

#[tokio::test(start_paused = true)]
async fn new_work_in_the_queue_cancels_the_countdown() {
    let rig = Rig::at(sunday(10, 0)).await;
    let e = rig.engine();
    e.create_queue(qc("Q", None, Some(sched(None, None, AfterDone::Sleep))))
        .await
        .unwrap();
    let mut events = e.subscribe();
    rig.add_slow(&url(0), "Q", MIB, MIB).await;
    next_finished(&mut events).await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    // More arrives 30 s in and takes 4 s: the first countdown is void.
    let id = rig.add_slow(&url(1), "Q", 4 * MIB, MIB).await;
    tokio::time::sleep(Duration::from_secs(40)).await;
    assert!(rig.power.actions().is_empty(), "the first countdown ran");
    assert_eq!(rig.state(&id).await, DownloadState::Completed);
    // The second finish announced its own, which runs 60 s after it.
    assert!(
        drain(&mut events)
            .iter()
            .any(|n| matches!(n, Notification::QueueFinished(_)))
    );
    tokio::time::sleep(Duration::from_secs(30)).await;
    assert_eq!(rig.power.actions(), vec![AfterDone::Sleep]);
}

#[tokio::test(start_paused = true)]
async fn stopping_or_pausing_is_not_finishing() {
    let rig = Rig::at(sunday(10, 0)).await;
    let e = rig.engine();
    e.create_queue(qc("Q", None, Some(sched(None, None, AfterDone::Shutdown))))
        .await
        .unwrap();
    e.create_queue(qc("None", None, Some(sched(None, None, AfterDone::None))))
        .await
        .unwrap();
    let mut events = e.subscribe();
    let a = rig.add_slow(&url(0), "Q", 8 * MIB, MIB).await;
    wait_for(e, &a, &[DownloadState::Downloading], LONG).await;
    e.stop_queue(name("Q")).await.unwrap();
    e.start_queue(name("Q")).await.unwrap();
    wait_for(e, &a, &[DownloadState::Downloading], LONG).await;
    e.pause(IdsParams {
        ids: vec![a.clone()],
    })
    .await
    .unwrap();
    // A queue whose action is "none" announces nothing.
    let b = rig.add_slow(&url(1), "None", MIB, MIB).await;
    finish(e, &b, LONG).await;
    tokio::time::sleep(Duration::from_secs(300)).await;
    assert!(
        !drain(&mut events)
            .iter()
            .any(|n| matches!(n, Notification::QueueFinished(_)))
    );
    assert!(rig.power.actions().is_empty());
}
