//! Queues and the scheduler (PROTOCOL.md "Queues and the scheduler"):
//! queue management, the clock tick that starts and stops queues and
//! applies speed windows, and after-done actions.

use std::time::Duration;

use whoosh_core::DownloadRecord;
use whoosh_proto::queue::{AFTER_DONE_GRACE_SECS, is_queue_name};
use whoosh_proto::{AfterDone, MAIN_QUEUE, QueueFinishedEvent, Schedule};

use super::*;
use crate::clock::LocalTime;
use crate::schedule::Edge;

/// Bounds of a named queue's `maxActive`.
const QUEUE_MAX_ACTIVE: std::ops::RangeInclusive<u32> = 1..=64;

/// A named queue.
#[derive(Debug, Clone)]
pub(super) struct QueueEntry {
    pub(super) max_active: u32,
    pub(super) schedule: Option<Schedule>,
    pub(super) running: bool,
    /// The number of the after-done action counting down, if any.
    pub(super) pending: Option<u64>,
}

impl QueueEntry {
    pub(super) fn new(max_active: u32, schedule: Option<Schedule>) -> Self {
        Self {
            max_active,
            schedule,
            running: true,
            pending: None,
        }
    }

    fn to_queue(&self, name: &str) -> Queue {
        Queue {
            name: name.to_string(),
            max_active: self.max_active,
            schedule: self.schedule.clone(),
            running: self.running,
        }
    }

    fn after_done(&self) -> AfterDone {
        self.schedule
            .as_ref()
            .map_or(AfterDone::None, |s| s.after_done)
    }
}

/// A queue name from the API: absent, empty or `"main"` is the main queue.
pub(super) fn normalize(name: Option<&str>) -> Option<String> {
    name.filter(|q| !q.trim().is_empty() && *q != MAIN_QUEUE)
        .map(str::to_string)
}

fn no_queue(name: &str) -> ServiceError {
    ServiceError::new(
        whoosh_proto::error_code::NOT_FOUND,
        format!("no queue named {name:?}"),
    )
}

fn check_name(name: &str) -> ServiceResult<()> {
    if !is_queue_name(name) {
        return Err(ServiceError::invalid_params(
            "a queue name is 1–64 characters, without control characters",
        ));
    }
    Ok(())
}

fn not_main(name: &str, what: &str) -> ServiceResult<()> {
    if name == MAIN_QUEUE {
        return Err(ServiceError::invalid_params(format!(
            "the main queue cannot be {what}"
        )));
    }
    Ok(())
}

fn check_config(c: &QueueConfig) -> ServiceResult<()> {
    if let Some(n) = c.max_active
        && !QUEUE_MAX_ACTIVE.contains(&n)
    {
        return Err(ServiceError::invalid_params(format!(
            "maxActive must be between 1 and 64, not {n}"
        )));
    }
    if let Some(s) = &c.schedule {
        schedule::check_schedule(s).map_err(ServiceError::invalid_params)?;
    }
    Ok(())
}

/// `queue.list`: the main queue first, then named queues by name.
pub(super) fn list(st: &St) -> Vec<Queue> {
    let main = Queue {
        name: MAIN_QUEUE.into(),
        max_active: st.settings.max_active_downloads,
        schedule: None,
        running: true,
    };
    std::iter::once(main)
        .chain(st.queues.iter().map(|(n, q)| q.to_queue(n)))
        .collect()
}

/// The stored named queues, plus a default one for every queue name a
/// download uses that is not stored (downloads put in a queue before
/// queues were stored). Returns whether any was added that way.
pub(super) async fn load(
    state: &dyn whoosh_core::StateStore,
    records: &[DownloadRecord],
) -> io::Result<(BTreeMap<String, QueueEntry>, bool)> {
    let mut queues = BTreeMap::new();
    if let Some(json) = state.get_value(QUEUES_KEY).await? {
        match serde_json::from_str::<Vec<Queue>>(&json) {
            Ok(stored) => {
                for q in stored {
                    if q.name == MAIN_QUEUE || !is_queue_name(&q.name) {
                        tracing::warn!(name = q.name, "ignoring a stored queue with a bad name");
                        continue;
                    }
                    let schedule = q.schedule.filter(|s| {
                        let ok = schedule::check_schedule(s).is_ok();
                        if !ok {
                            tracing::warn!(name = q.name, "ignoring a stored queue's bad schedule");
                        }
                        ok
                    });
                    let mut e = QueueEntry::new(
                        q.max_active
                            .clamp(*QUEUE_MAX_ACTIVE.start(), *QUEUE_MAX_ACTIVE.end()),
                        schedule,
                    );
                    e.running = q.running;
                    queues.insert(q.name, e);
                }
            }
            Err(e) => tracing::warn!("stored queues unreadable, starting without: {e}"),
        }
    }
    let mut adopted = false;
    for r in records {
        if let Some(q) = &r.queue
            && !queues.contains_key(q)
        {
            queues.insert(q.clone(), QueueEntry::new(1, None));
            adopted = true;
        }
    }
    Ok((queues, adopted))
}

/// Looks at the wall clock every `Tuning::clock_tick`.
pub(super) async fn scheduler(weak: Weak<Inner>) {
    let every = match weak.upgrade() {
        Some(i) => i.config.tuning.clock_tick,
        None => return,
    };
    loop {
        {
            let Some(inner) = weak.upgrade() else { return };
            if inner.closed.load(Ordering::Acquire) {
                return;
            }
            inner.clock_tick().await;
        }
        tokio::time::sleep(every).await;
    }
}

/// After the grace period, run after-done action number `number` unless
/// it was cancelled (or the queue ran again) meanwhile.
async fn countdown(weak: Weak<Inner>, number: u64, action: AfterDone) {
    tokio::time::sleep(Duration::from_secs(AFTER_DONE_GRACE_SECS)).await;
    let Some(inner) = weak.upgrade() else { return };
    if inner.shutting_down.load(Ordering::Acquire) {
        return;
    }
    let name = {
        let mut st = lock(&inner.st);
        let Some((name, q)) = st
            .queues
            .iter_mut()
            .find(|(_, q)| q.pending == Some(number))
        else {
            return;
        };
        q.pending = None;
        name.clone()
    };
    // Whatever happens to the machine next, the records are on disk.
    if let Err(e) = inner.deps.state.sync().await {
        tracing::warn!("state sync before the after-done action failed: {e}");
    }
    tracing::info!(
        queue = name,
        ?action,
        "queue finished: running its after-done action"
    );
    if let Err(e) = inner.config.power.run(action).await {
        tracing::warn!(queue = name, ?action, "after-done action failed: {e}");
    }
}

impl Inner {
    pub(super) fn emit_queues(&self) {
        let v = list(&lock(&self.st));
        self.bus.emit(Notification::Queues(v));
    }

    /// Persist the named queues (small; rewritten whole).
    pub(super) async fn save_queues(&self) {
        let _g = self.save_lock.lock().await;
        let v: Vec<Queue> = lock(&self.st)
            .queues
            .iter()
            .map(|(n, q)| q.to_queue(n))
            .collect();
        let json = serde_json::to_string(&v).unwrap_or_else(|_| "[]".into());
        if let Err(e) = self.deps.state.set_value(QUEUES_KEY, &json).await {
            tracing::warn!("saving queues failed: {e}");
        }
    }

    /// Make the speed limit in force the one `now` calls for.
    pub(super) fn apply_speed_limit(&self, now: LocalTime) {
        let mut st = lock(&self.st);
        let limit = schedule::speed_limit(
            &st.settings.speed_schedule,
            now,
            st.settings.speed_limit_bps,
        );
        if st.speed_limit != limit {
            tracing::info!(?limit, "speed limit now in force");
            st.speed_limit = limit;
        }
        // Always: cheap, and `set_settings` relies on it.
        self.limiter.set_rate(limit);
    }

    /// One look at the wall clock: speed windows, scheduled starts and stops.
    pub(super) async fn clock_tick(self: &Arc<Self>) {
        if self.shutting_down.load(Ordering::Acquire) {
            return;
        }
        let now = self.config.clock.local();
        self.apply_speed_limit(now);
        let edges = {
            let mut guard = lock(&self.st);
            let st = &mut *guard;
            let scheduled = st
                .queues
                .iter()
                .filter_map(|(n, q)| q.schedule.as_ref().map(|s| (n.as_str(), s)));
            st.crossings.advance(now, scheduled)
        };
        for (name, edge) in edges {
            tracing::info!(queue = name, ?edge, %now, "scheduled queue event");
            // Not waited for: a held run stops on its own.
            let _ = self.set_running(&name, edge == Edge::Start).await;
        }
    }

    /// Start or stop a named queue. Stopping holds its running downloads
    /// (they go back to `queued`); returns receivers that turn true once
    /// each has stopped.
    async fn set_running(
        self: &Arc<Self>,
        name: &str,
        running: bool,
    ) -> ServiceResult<Vec<watch::Receiver<bool>>> {
        let (changed, waits) = {
            let mut guard = lock(&self.st);
            let st = &mut *guard;
            let q = st.queues.get_mut(name).ok_or_else(|| no_queue(name))?;
            let changed = q.running != running;
            q.running = running;
            let waits = if running {
                Vec::new()
            } else {
                hold(st.downloads.values(), name)
            };
            (changed, waits)
        };
        if changed {
            self.save_queues().await;
            self.emit_queues();
        }
        self.pump();
        Ok(waits)
    }

    /// A download of `queue` ran out (completed or failed): if nothing in
    /// the queue is left to run and it has an after-done action, announce
    /// the action and start its countdown.
    pub(super) fn check_finished(self: &Arc<Self>, queue: &str) {
        let (number, action) = {
            let mut guard = lock(&self.st);
            let st = &mut *guard;
            let left = st.downloads.values().any(|e| {
                e.record.queue.as_deref() == Some(queue)
                    && !e.removing
                    && (e.run.is_some() || e.record.state == DownloadState::Queued)
            });
            let Some(q) = st.queues.get_mut(queue) else {
                return;
            };
            let action = q.after_done();
            if left || !q.running || action == AfterDone::None || q.pending.is_some() {
                return;
            }
            st.next_action += 1;
            q.pending = Some(st.next_action);
            (st.next_action, action)
        };
        let at_ms = self.config.clock.now_ms() + (AFTER_DONE_GRACE_SECS * 1000) as i64;
        tracing::info!(
            queue,
            ?action,
            at_ms,
            "queue finished; after-done action pending"
        );
        self.bus
            .emit(Notification::QueueFinished(QueueFinishedEvent {
                name: queue.to_string(),
                action,
                at_ms,
            }));
        tokio::spawn(countdown(Arc::downgrade(self), number, action));
    }

    // ------------------------------------------------------- service ops

    pub(super) async fn create_queue(self: &Arc<Self>, c: QueueConfig) -> ServiceResult<Queue> {
        check_name(&c.name)?;
        check_config(&c)?;
        let q = {
            let mut st = lock(&self.st);
            if c.name == MAIN_QUEUE || st.queues.contains_key(&c.name) {
                return Err(ServiceError::invalid_params(format!(
                    "a queue named {:?} already exists",
                    c.name
                )));
            }
            let mut e = QueueEntry::new(c.max_active.unwrap_or(1), c.schedule);
            // A queue with a start time waits for it.
            e.running = e.schedule.as_ref().is_none_or(|s| s.start.is_none());
            let q = e.to_queue(&c.name);
            st.queues.insert(c.name, e);
            q
        };
        self.save_queues().await;
        self.emit_queues();
        Ok(q)
    }

    pub(super) async fn update_queue(self: &Arc<Self>, c: QueueConfig) -> ServiceResult<Queue> {
        if c.name == MAIN_QUEUE {
            if c.schedule.is_some() {
                return Err(ServiceError::invalid_params(
                    "the main queue has no schedule; use a named queue",
                ));
            }
            if let Some(n) = c.max_active {
                let mut s = lock(&self.st).settings.clone();
                s.max_active_downloads = n;
                self.set_settings(s).await?;
            }
            self.emit_queues();
            return Ok(list(&lock(&self.st)).swap_remove(0));
        }
        check_config(&c)?;
        let q = {
            let mut st = lock(&self.st);
            let q = st
                .queues
                .get_mut(&c.name)
                .ok_or_else(|| no_queue(&c.name))?;
            if let Some(n) = c.max_active {
                q.max_active = n;
            }
            q.schedule = c.schedule;
            q.to_queue(&c.name)
        };
        self.save_queues().await;
        self.emit_queues();
        // A larger maxActive may start more.
        self.pump();
        Ok(q)
    }

    pub(super) async fn rename_queue(
        self: &Arc<Self>,
        p: QueueRenameParams,
    ) -> ServiceResult<Queue> {
        not_main(&p.name, "renamed")?;
        check_name(&p.new_name)?;
        let (q, moved) = {
            let mut st = lock(&self.st);
            if !st.queues.contains_key(&p.name) {
                return Err(no_queue(&p.name));
            }
            if p.new_name == p.name {
                return Ok(st.queues[&p.name].to_queue(&p.name));
            }
            if p.new_name == MAIN_QUEUE || st.queues.contains_key(&p.new_name) {
                return Err(ServiceError::invalid_params(format!(
                    "a queue named {:?} already exists",
                    p.new_name
                )));
            }
            let e = st.queues.remove(&p.name).expect("checked");
            let q = e.to_queue(&p.new_name);
            st.queues.insert(p.new_name.clone(), e);
            let moved = move_downloads(&mut st, &p.name, Some(&p.new_name));
            (q, moved)
        };
        self.after_move(&moved).await;
        self.save_queues().await;
        self.emit_queues();
        Ok(q)
    }

    pub(super) async fn remove_queue(self: &Arc<Self>, name: String) -> ServiceResult<()> {
        not_main(&name, "removed")?;
        let moved = {
            let mut st = lock(&self.st);
            if st.queues.remove(&name).is_none() {
                return Err(no_queue(&name));
            }
            move_downloads(&mut st, &name, None)
        };
        self.after_move(&moved).await;
        self.save_queues().await;
        self.emit_queues();
        self.pump();
        Ok(())
    }

    pub(super) async fn start_queue(self: &Arc<Self>, name: String) -> ServiceResult<()> {
        if name == MAIN_QUEUE {
            return Ok(());
        }
        self.check_open()?;
        self.set_running(&name, true).await.map(drop)
    }

    pub(super) async fn stop_queue(self: &Arc<Self>, name: String) -> ServiceResult<()> {
        if name == MAIN_QUEUE {
            return Err(ServiceError::invalid_params(
                "the main queue cannot be stopped; pause its downloads",
            ));
        }
        let waits = self.set_running(&name, false).await?;
        wait_all(waits).await;
        Ok(())
    }

    pub(super) fn cancel_queue_action(&self, name: &str) -> ServiceResult<()> {
        if name == MAIN_QUEUE {
            return Ok(());
        }
        let mut st = lock(&self.st);
        let q = st.queues.get_mut(name).ok_or_else(|| no_queue(name))?;
        if q.pending.take().is_some() {
            tracing::info!(queue = name, "after-done action cancelled");
        }
        Ok(())
    }

    pub(super) async fn set_queue(self: &Arc<Self>, p: SetQueueParams) -> ServiceResult<()> {
        self.validate_ids(&p.ids)?;
        let target = normalize(p.queue.as_deref());
        let moved = {
            let mut guard = lock(&self.st);
            let st = &mut *guard;
            let stopped = match &target {
                Some(q) => !st.queues.get(q).ok_or_else(|| no_queue(q))?.running,
                None => false,
            };
            let mut moved = Vec::new();
            for id in &p.ids {
                let Some(e) = st.downloads.get_mut(id) else {
                    continue;
                };
                if e.record.queue == target {
                    continue;
                }
                e.record.queue = target.clone();
                moved.push(id.clone());
            }
            if stopped && let Some(q) = &target {
                // Joining a stopped queue: wait with the rest of it.
                hold(st.downloads.values(), q);
            }
            moved
        };
        self.after_move(&moved).await;
        self.pump();
        Ok(())
    }

    /// Persist and announce downloads whose queue changed.
    async fn after_move(&self, ids: &[DownloadId]) {
        for id in ids {
            self.save(id).await;
            self.announce(id);
        }
    }
}

/// Ask every running download of `queue` to stop and wait queued (unless
/// it is already stopping for another reason).
fn hold<'a>(entries: impl Iterator<Item = &'a Entry>, queue: &str) -> Vec<watch::Receiver<bool>> {
    entries
        .filter(|e| e.record.queue.as_deref() == Some(queue))
        .filter_map(|e| {
            let run = e.run.as_ref()?;
            if run.stop.borrow().is_none() {
                run.stop.send_replace(Some(StopReason::Hold));
            }
            Some(run.done.clone())
        })
        .collect()
}

/// Move every download of queue `from` to `to` (`None` = the main queue);
/// returns their ids.
fn move_downloads(st: &mut St, from: &str, to: Option<&str>) -> Vec<DownloadId> {
    st.downloads
        .values_mut()
        .filter(|e| e.record.queue.as_deref() == Some(from))
        .map(|e| {
            e.record.queue = to.map(str::to_string);
            e.record.id.clone()
        })
        .collect()
}
