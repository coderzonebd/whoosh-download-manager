//! The API as a Rust trait, without transport or authentication.
//!
//! `whoosh-engine` implements [`Service`]; `whoosh-daemon` serves any `Service`
//! over JSON-RPC (PROTOCOL.md) and only translates. A mock `Service` lets
//! the daemon, the CLI and the UIs be built before the engine exists.

use futures::future::BoxFuture;
use futures::stream::BoxStream;
use serde::Serialize;

use crate::{
    AddParams, AddResult, DownloadDetail, DownloadSummary, IdParams, IdsParams, ListParams,
    PreconnectParams, ProbeParams, ProbeResult, ProgressEvent, Queue, QueueConfig,
    QueueFinishedEvent, QueueNameParams, QueueRenameParams, RefreshUrlParams, RemoveParams,
    RemovedEvent, SetQueueParams, Settings, StateEvent, Stats, error_code, event,
};

/// A method failed. `code` is a JSON-RPC error code: [`error_code`], or
/// `-32602` (invalid params) for input the engine rejects.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ServiceError {
    pub code: i32,
    pub message: String,
}

impl ServiceError {
    pub const INVALID_PARAMS: i32 = -32602;

    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn not_found(id: &str) -> Self {
        Self::new(error_code::NOT_FOUND, format!("no download with id {id}"))
    }

    pub fn invalid_state(message: impl Into<String>) -> Self {
        Self::new(error_code::INVALID_STATE, message)
    }

    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(Self::INVALID_PARAMS, message)
    }

    pub fn io(message: impl Into<String>) -> Self {
        Self::new(error_code::IO, message)
    }

    pub fn network(message: impl Into<String>) -> Self {
        Self::new(error_code::NETWORK, message)
    }

    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::new(error_code::UNSUPPORTED, message)
    }
}

pub type ServiceResult<T> = Result<T, ServiceError>;

/// A daemon → client notification (PROTOCOL.md §4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notification {
    DownloadAdded(DownloadSummary),
    DownloadUpdated(DownloadSummary),
    DownloadProgress(ProgressEvent),
    DownloadState(StateEvent),
    DownloadRemoved(RemovedEvent),
    Stats(Stats),
    Settings(Settings),
    Queues(Vec<Queue>),
    QueueFinished(QueueFinishedEvent),
}

impl Notification {
    /// The JSON-RPC method name.
    pub fn method(&self) -> &'static str {
        match self {
            Self::DownloadAdded(_) => event::DOWNLOAD_ADDED,
            Self::DownloadUpdated(_) => event::DOWNLOAD_UPDATED,
            Self::DownloadProgress(_) => event::DOWNLOAD_PROGRESS,
            Self::DownloadState(_) => event::DOWNLOAD_STATE,
            Self::DownloadRemoved(_) => event::DOWNLOAD_REMOVED,
            Self::Stats(_) => event::STATS,
            Self::Settings(_) => event::SETTINGS,
            Self::Queues(_) => event::QUEUES,
            Self::QueueFinished(_) => event::QUEUE_FINISHED,
        }
    }

    /// The JSON-RPC params.
    pub fn params(&self) -> serde_json::Value {
        fn json(v: &impl Serialize) -> serde_json::Value {
            serde_json::to_value(v).unwrap_or(serde_json::Value::Null)
        }
        match self {
            Self::DownloadAdded(v) => json(v),
            Self::DownloadUpdated(v) => json(v),
            Self::DownloadProgress(v) => json(v),
            Self::DownloadState(v) => json(v),
            Self::DownloadRemoved(v) => json(v),
            Self::Stats(v) => json(v),
            Self::Settings(v) => json(v),
            Self::Queues(v) => json(v),
            Self::QueueFinished(v) => json(v),
        }
    }

    /// Progress and stats are superseded by the next one of their kind, so a
    /// slow subscriber may skip them. Everything else must be delivered.
    pub fn is_lossy(&self) -> bool {
        matches!(self, Self::DownloadProgress(_) | Self::Stats(_))
    }
}

/// Every method of PROTOCOL.md §3 except `session.hello` (the daemon's own).
pub trait Service: Send + Sync {
    /// Advertised in `HelloResult::capabilities`, e.g. `["http", "https", "h2", "h3"]`.
    fn capabilities(&self) -> Vec<String>;

    fn probe(&self, params: ProbeParams) -> BoxFuture<'_, ServiceResult<ProbeResult>>;
    fn add(&self, params: AddParams) -> BoxFuture<'_, ServiceResult<AddResult>>;
    fn list(&self, params: ListParams) -> BoxFuture<'_, ServiceResult<Vec<DownloadSummary>>>;
    fn get(&self, params: IdParams) -> BoxFuture<'_, ServiceResult<DownloadDetail>>;
    fn pause(&self, params: IdsParams) -> BoxFuture<'_, ServiceResult<()>>;
    fn resume(&self, params: IdsParams) -> BoxFuture<'_, ServiceResult<()>>;
    fn remove(&self, params: RemoveParams) -> BoxFuture<'_, ServiceResult<()>>;
    fn refresh_url(&self, params: RefreshUrlParams) -> BoxFuture<'_, ServiceResult<()>>;
    fn preconnect(&self, params: PreconnectParams) -> BoxFuture<'_, ServiceResult<()>>;
    fn stats(&self) -> BoxFuture<'_, ServiceResult<Stats>>;
    fn settings(&self) -> BoxFuture<'_, ServiceResult<Settings>>;
    fn set_settings(&self, settings: Settings) -> BoxFuture<'_, ServiceResult<Settings>>;

    // Queues and the scheduler. A service that has them advertises the
    // `queues` capability; until then these answer "unsupported".

    fn set_queue(&self, _params: SetQueueParams) -> BoxFuture<'_, ServiceResult<()>> {
        unsupported("download.setQueue")
    }
    fn queues(&self) -> BoxFuture<'_, ServiceResult<Vec<Queue>>> {
        unsupported("queue.list")
    }
    fn create_queue(&self, _config: QueueConfig) -> BoxFuture<'_, ServiceResult<Queue>> {
        unsupported("queue.create")
    }
    fn update_queue(&self, _config: QueueConfig) -> BoxFuture<'_, ServiceResult<Queue>> {
        unsupported("queue.update")
    }
    fn rename_queue(&self, _params: QueueRenameParams) -> BoxFuture<'_, ServiceResult<Queue>> {
        unsupported("queue.rename")
    }
    fn remove_queue(&self, _params: QueueNameParams) -> BoxFuture<'_, ServiceResult<()>> {
        unsupported("queue.remove")
    }
    fn start_queue(&self, _params: QueueNameParams) -> BoxFuture<'_, ServiceResult<()>> {
        unsupported("queue.start")
    }
    fn stop_queue(&self, _params: QueueNameParams) -> BoxFuture<'_, ServiceResult<()>> {
        unsupported("queue.stop")
    }
    fn cancel_queue_action(&self, _params: QueueNameParams) -> BoxFuture<'_, ServiceResult<()>> {
        unsupported("queue.cancelAction")
    }

    /// A new, independent subscription to every notification (one per client
    /// connection). Ends when the service shuts down. Notifications for
    /// which [`Notification::is_lossy`] is false are never dropped; lossy
    /// ones may be skipped or coalesced for a subscriber that falls behind.
    fn subscribe(&self) -> BoxStream<'static, Notification>;

    /// Stop accepting work, pause transfers, flush every journal and the
    /// state store. The daemon calls this once on shutdown (SIGTERM, Ctrl-C,
    /// service stop) and exits when it resolves.
    fn shutdown(&self) -> BoxFuture<'_, ()>;
}

fn unsupported<T: Send + 'static>(method: &str) -> BoxFuture<'static, ServiceResult<T>> {
    let err = ServiceError::unsupported(format!("{method} is not supported by this daemon yet"));
    Box::pin(async move { Err(err) })
}
